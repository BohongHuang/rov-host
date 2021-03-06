/* param_tuner.rs
 *
 * Copyright 2021-2022 Bohong Huang
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program. If not, see <http://www.gnu.org/licenses/>.
 */

use std::{fmt::Debug, cmp::{max, min}, collections::{HashMap, VecDeque}, ops::Deref, time::{SystemTime, Duration}, io::Error as IOError};
use async_std::{net::TcpStream, task, prelude::*};

use glib::{Sender, clone};
use gtk::{Align, Box as GtkBox, Button, Image, Inhibit, Label, Orientation, SpinButton, Switch, prelude::*, FlowBox, Scale, SelectionMode};
use adw::{HeaderBar, PreferencesGroup, PreferencesPage, PreferencesWindow, prelude::*, Clamp, Leaflet, ToastOverlay, ExpanderRow, ActionRow};
use relm4::{factory::{FactoryPrototype, FactoryVec}, send, MicroWidgets, MicroModel};
use relm4_macros::micro_widget;

use serde::{Serialize, Deserialize};
use derivative::*;

use crate::ui::graph_view::{GraphView, Point as GraphPoint};
use crate::slave::SlaveTcpMsg;
use crate::function::*;

use super::SlaveMsg;

pub enum SlaveParameterTunerMsg {
    SetPropellerLowerDeadzone(usize, i8),
    SetPropellerUpperDeadzone(usize, i8),
    SetPropellerPowerPositive(usize, f64),
    SetPropellerPowerNegative(usize, f64),
    SetPropellerReversed(usize, bool),
    SetPropellerEnabled(usize, bool),
    SetP(usize, f64),
    SetI(usize, f64),
    SetD(usize, f64),
    SetPropellerPwmFreqCalibration(f64),
    ResetParameters,
    ApplyParameters,
    StartDebug(TcpStream),
    StopDebug,
    FeedbacksReceived(SlaveParameterTunerFeedbackPacket),
    ParametersReceived(SlaveParameterTunerPacket),
}

#[tracker::track(pub)]
#[derive(Debug, Derivative, PartialEq, Clone)]
#[derivative(Default)]
pub struct PropellerModel {
    key: String,
    deadzone_lower: i8,
    deadzone_upper: i8,
    #[derivative(Default(value="0.75"))]
    power_positive: f64,
    #[derivative(Default(value="0.75"))]
    power_negative: f64,
    #[derivative(Default(value="true"))]
    enabled: bool,
    reversed: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
struct Propeller {
    pub deadzone_lower: i8,
    pub deadzone_upper: i8,
    pub power_positive: f64,
    pub power_negative: f64,
    pub reversed: bool,
    pub enabled: bool,
}

const DEFAULT_PROPELLERS: [&'static str; 6] = ["front_left", "front_right", "back_left", "back_right", "center_left", "center_right"];
const DEFAULT_CONTROL_LOOPS: [&'static str; 2] = ["depth_lock", "direction_lock"];
const CARD_MIN_WIDTH: i32 = 300;

trait SlaveParameterTunerWindowExt {
    fn set_destroy(&self, destroy: bool);
}

impl SlaveParameterTunerWindowExt for PreferencesWindow {
    fn set_destroy(&self, destroy: bool) {
        if destroy {
            self.destroy();
        }
    }
}

impl PropellerModel {
    pub fn new(key: &str) -> PropellerModel {
        let a = PreferencesWindow::new();
        a.set_destroy(false);
        PropellerModel {
            key: key.to_string(),
            ..Default::default()
        }
    }
    
    fn vec_to_map(v: Vec<&PropellerModel>) -> HashMap<String, Propeller> {
        v.iter().map(|model| {
            let PropellerModel { key, deadzone_lower, deadzone_upper, power_positive, power_negative, reversed, enabled, .. } = Deref::deref(model).clone();
            (key, Propeller { deadzone_lower, deadzone_upper, power_positive, power_negative, reversed, enabled })
        }).collect()
    }

    fn key_to_string<'a, 'b : 'a>(key: &'b str) -> &'a str {
        match key {
            "front_left"   => "??????",
            "front_right"  => "??????",
            "back_left"    => "??????",
            "back_right"   => "??????",
            "center_left"  => "??????",
            "center_right" => "??????",
            key => key,
        }
    }
}

#[tracker::track(pub)]
#[derive(Debug, Derivative, PartialEq, Clone)]
#[derivative(Default)]
pub struct ControlLoopModel {
    key: String,
    #[derivative(Default(value="1.0"))]
    p: f64,
    #[derivative(Default(value="1.0"))]
    i: f64,
    #[derivative(Default(value="1.0"))]
    d: f64,
    feedbacks: VecDeque<f32>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
struct ControlLoop {
    pub p: f64,
    pub i: f64,
    pub d: f64,
}

impl ControlLoopModel {
    fn new(key: &str) -> ControlLoopModel {
        ControlLoopModel {
            key: key.to_string(),
            ..Default::default()
        }
    }
    
    fn vec_to_map(v: Vec<&ControlLoopModel>) -> HashMap<String, ControlLoop> {
        v.iter().map(Deref::deref).map(Self::to_control_loop).collect()
    }

    fn key_to_string<'a, 'b : 'a>(key: &'b str) -> &'a str {
        match key {
            "depth_lock"     => "????????????", 
            "direction_lock" => "????????????",
            key => key,
        }
    }

    fn to_control_loop(&self) -> (String, ControlLoop) {
        let Self { key, p, i, d, .. } = self.clone();
        (key, ControlLoop { p, i, d })
    }
}

#[tracker::track(pub)]
#[derive(Debug, Derivative)]
#[derivative(Default)]
pub struct SlaveParameterTunerModel {
    #[derivative(Default(value="0.0"))]
    propeller_pwm_frequency_calibration: f64,
    #[no_eq]
    #[derivative(Default(value="FactoryVec::new()"))]
    propellers: FactoryVec<PropellerModel>,
    #[no_eq]
    #[derivative(Default(value="FactoryVec::new()"))]
    control_loops: FactoryVec<ControlLoopModel>,
    #[no_eq]
    tcp_msg_sender: Option<async_std::channel::Sender<SlaveParameterTunerTcpMsg>>,
    graph_view_point_num_limit: u16,
    stopped: bool,
}

#[relm4::factory_prototype(pub)]
impl FactoryPrototype for PropellerModel {
    type Factory = FactoryVec<Self>;
    type Widgets = PropellerConfigWidgets;
    type View = FlowBox;
    type Msg = SlaveParameterTunerMsg;

    view! {
        group = &PreferencesGroup {
            set_title: PropellerModel::key_to_string(&self.key),
            add = &GtkBox {
                set_orientation: Orientation::Vertical,
                set_spacing: 12,
                append = &PreferencesGroup {
                    add = &ExpanderRow {
                        set_title: "??????",
                        set_show_enable_switch: true,
                        set_expanded: *self.get_enabled(),
                        set_enable_expansion: track!(self.changed(PropellerModel::enabled()), *self.get_enabled()),
                        connect_enable_expansion_notify(sender, key) => move |expander| {
                            send!(sender, SlaveParameterTunerMsg::SetPropellerEnabled(key, expander.enables_expansion()));
                        },
                        add_row = &ActionRow {
                            set_title: "??????",
                            add_suffix: reversed_switch = &Switch {
                                set_valign: Align::Center,
                                set_active: track!(self.changed(PropellerModel::reversed()), *self.get_reversed()),
                                connect_state_set(sender, key) => move |_switch, state| {
                                    send!(sender, SlaveParameterTunerMsg::SetPropellerReversed(key, state));
                                    Inhibit(false)
                                }
                            },
                            set_activatable_widget: Some(&reversed_switch),
                        },
                        add_row = &ActionRow {
                            set_title: "????????????",
                            add_suffix = &SpinButton::with_range(0.01, 1.0, 0.01) {
                                set_value: track!(self.changed(PropellerModel::power_positive()), *self.get_power_positive()),
                                set_digits: 2,
                                set_valign: Align::Center,
                                connect_value_changed(key, sender) => move |button| {
                                    send!(sender, SlaveParameterTunerMsg::SetPropellerPowerPositive(key, button.value()));
                                }
                            },
                        },
                        add_row = &ActionRow {
                            set_child = Some(&Scale::with_range(Orientation::Horizontal, 0.01, 1.0, 0.01)) {
                                set_width_request: CARD_MIN_WIDTH,
                                set_round_digits: 2,
                                set_value: track!(self.changed(PropellerModel::power_positive()), *self.get_power_positive() as f64),
                                connect_value_changed(key, sender) => move |scale| {
                                    send!(sender, SlaveParameterTunerMsg::SetPropellerPowerPositive(key, scale.value()));
                                }
                            }
                        },
                        add_row = &ActionRow {
                            set_title: "????????????",
                            add_suffix = &SpinButton::with_range(0.01, 1.0, 0.01) {
                                set_value: track!(self.changed(PropellerModel::power_negative()), *self.get_power_negative()),
                                set_digits: 2,
                                set_valign: Align::Center,
                                connect_value_changed(key, sender) => move |button| {
                                    send!(sender, SlaveParameterTunerMsg::SetPropellerPowerNegative(key, button.value()));
                                }
                            },
                        },
                        add_row = &ActionRow {
                            set_child = Some(&Scale::with_range(Orientation::Horizontal, 0.01, 1.0, 0.01)) {
                                set_width_request: CARD_MIN_WIDTH,
                                set_round_digits: 2,
                                set_value: track!(self.changed(PropellerModel::power_negative()), *self.get_power_negative() as f64),
                                connect_value_changed(key, sender) => move |scale| {
                                    send!(sender, SlaveParameterTunerMsg::SetPropellerPowerNegative(key, scale.value()));
                                }
                            }
                        },
                        add_row = &ActionRow {
                            set_title: "????????????",
                            add_suffix = &SpinButton::with_range(-128.0, 127.0, 1.0) {
                                set_value: track!(self.changed(PropellerModel::deadzone_upper()), *self.get_deadzone_upper() as f64),
                                set_digits: 0,
                                set_valign: Align::Center,
                                connect_value_changed(key, sender) => move |button| {
                                    send!(sender, SlaveParameterTunerMsg::SetPropellerUpperDeadzone(key, button.value() as i8));
                                }
                            },
                        },
                        add_row = &ActionRow {
                            set_child = Some(&Scale::with_range(Orientation::Horizontal, -128.0, 127.0, 1.0)) {
                                set_width_request: CARD_MIN_WIDTH,
                                set_round_digits: 0,
                                set_value: track!(self.changed(PropellerModel::deadzone_upper()), *self.get_deadzone_upper() as f64),
                                connect_value_changed(key, sender) => move |scale| {
                                    send!(sender, SlaveParameterTunerMsg::SetPropellerUpperDeadzone(key, scale.value() as i8));
                                }
                            }
                        },
                        add_row = &ActionRow {
                            set_title: "????????????",
                            add_suffix = &SpinButton::with_range(-128.0, 127.0, 1.0) {
                                set_value: track!(self.changed(PropellerModel::deadzone_lower()), *self.get_deadzone_lower() as f64),
                                set_digits: 0,
                                set_valign: Align::Center,
                                connect_value_changed(key, sender) => move |button| {
                                    send!(sender, SlaveParameterTunerMsg::SetPropellerLowerDeadzone(key, button.value() as i8));
                                }
                            },
                        },
                        add_row = &ActionRow {
                            set_child = Some(&Scale::with_range(Orientation::Horizontal, -128.0, 127.0, 1.0)) {
                                set_width_request: CARD_MIN_WIDTH,
                                set_round_digits: 0,
                                set_value: track!(self.changed(PropellerModel::deadzone_lower()), *self.get_deadzone_lower() as f64),
                                connect_value_changed(key, sender) => move |scale| {
                                    send!(sender, SlaveParameterTunerMsg::SetPropellerLowerDeadzone(key, scale.value() as i8));
                                }
                            }
                        },
                    },
                },
            }
        }
    }

    fn position(&self, _index: &usize) {
        
    }
}

#[relm4::factory_prototype(pub)]
impl FactoryPrototype for ControlLoopModel {
    type Factory = FactoryVec<Self>;
    type Widgets = ControlLoopWidgets;
    type View = FlowBox;
    type Msg = SlaveParameterTunerMsg;
    
    view! {
        group = &PreferencesGroup {
            set_title: ControlLoopModel::key_to_string(&self.key),
            add = &GtkBox {
                set_orientation: Orientation::Vertical,
                set_spacing: 12,
                append = &PreferencesGroup {
                    add = &ActionRow {
                        set_child = Some(&GraphView::new()) {
                            set_width_request: CARD_MIN_WIDTH,
                            set_height_request: CARD_MIN_WIDTH / 2,
                            set_points: track!(self.changed(ControlLoopModel::feedbacks()), self.feedbacks.iter().map(|&x|  GraphPoint { value: x * 100.0 }).collect()),
                            set_upper_value: 100.0,
                            set_lower_value: -100.0,
                        },
                    },
                },
                append = &PreferencesGroup {
                    add = &ActionRow {
                        set_title: "P",
                        add_suffix = &SpinButton::with_range(0.0, 100.0, 0.01) {
                            set_value: track!(self.changed(ControlLoopModel::p()), *self.get_p()),
                            set_digits: 2,
                            set_valign: Align::Center,
                            connect_value_changed(key, sender) => move |button| {
                                send!(sender, SlaveParameterTunerMsg::SetP(key, button.value()));
                            }
                        },
                    },
                    add = &ActionRow {
                        set_child = Some(&Scale::with_range(Orientation::Horizontal, 0.0, 100.0, 0.01)) {
                            set_width_request: CARD_MIN_WIDTH,
                            set_round_digits: 2,
                            set_value: track!(self.changed(ControlLoopModel::p()), *self.get_p()),
                            connect_value_changed(key, sender) => move |scale| {
                                send!(sender, SlaveParameterTunerMsg::SetP(key, scale.value()));
                            }
                        }
                    },
                },
                append = &PreferencesGroup {
                    add = &ActionRow {
                        set_title: "I",
                        add_suffix = &SpinButton::with_range(0.0, 100.0, 0.01) {
                            set_value: track!(self.changed(ControlLoopModel::i()), *self.get_i()),
                            set_digits: 2,
                            set_valign: Align::Center,
                            connect_value_changed(key, sender) => move |button| {
                                send!(sender, SlaveParameterTunerMsg::SetI(key, button.value()));
                            }
                        },
                    },
                    add = &ActionRow {
                        set_child = Some(&Scale::with_range(Orientation::Horizontal, 0.0, 100.0, 0.01)) {
                            set_width_request: CARD_MIN_WIDTH,
                            set_round_digits: 2,
                            set_value: track!(self.changed(ControlLoopModel::i()), *self.get_i()),
                            connect_value_changed(key, sender) => move |scale| {
                                send!(sender, SlaveParameterTunerMsg::SetI(key, scale.value()));
                            }
                        }
                    },
                },
                append = &PreferencesGroup {
                    add = &ActionRow {
                        set_title: "D",
                        add_suffix = &SpinButton::with_range(0.0, 100.0, 0.01) {
                            set_value: track!(self.changed(ControlLoopModel::d()), *self.get_d()),
                            set_digits: 2,
                            set_valign: Align::Center,
                            connect_value_changed(key, sender) => move |button| {
                                send!(sender, SlaveParameterTunerMsg::SetD(key, button.value()));
                            }
                        },
                    },
                    add = &ActionRow {
                        set_child = Some(&Scale::with_range(Orientation::Horizontal, 0.0, 100.0, 0.01)) {
                            set_width_request: CARD_MIN_WIDTH,
                            set_round_digits: 2,
                            set_value: track!(self.changed(ControlLoopModel::d()), *self.get_d()),
                            connect_value_changed(key, sender) => move |scale| {
                                send!(sender, SlaveParameterTunerMsg::SetD(key, scale.value()));
                            }
                        }
                    },
                },
            }
        }
    }
    
    fn position(&self, _index: &usize) {
        
    }
}

impl SlaveParameterTunerModel {
    pub fn new(graph_view_point_num_limit: u16) -> Self {
        SlaveParameterTunerModel {
            propellers: FactoryVec::from_vec(DEFAULT_PROPELLERS.iter().map(|key| PropellerModel::new(key)).collect()),
            control_loops: FactoryVec::from_vec(DEFAULT_CONTROL_LOOPS.iter().map(|key| ControlLoopModel::new(key)).collect()),
            graph_view_point_num_limit,
            ..Default::default()
        }
    }
}

#[micro_widget(pub)]
impl MicroWidgets<SlaveParameterTunerModel> for SlaveParameterTunerWidgets {
    view! {
        window = PreferencesWindow {
            set_destroy_with_parent: true,
            set_modal: true,
            set_search_enabled: false,
            add = &PreferencesPage {
                set_title: "?????????",
                set_icon_name: Some("weather-windy-symbolic"),
                set_hexpand: true,
                set_vexpand: true,
                set_can_focus: false,
                add: group_pwm = &PreferencesGroup {
                    set_title: "PWM ?????????",
                    add = &FlowBox {
                        set_activate_on_single_click: false,
                        set_valign: Align::Start,
                        set_row_spacing: 12,
                        set_selection_mode: SelectionMode::None,
                        insert(-1) = &PreferencesGroup {
                            add = &ActionRow {
                                set_title: "????????????",
                                add_suffix = &SpinButton::with_range(-0.1, 0.1, 0.0001) {
                                    set_value: track!(model.changed(SlaveParameterTunerModel::propeller_pwm_frequency_calibration()), *model.get_propeller_pwm_frequency_calibration() as f64),
                                    set_digits: 4,
                                    set_valign: Align::Center,
                                    connect_value_changed(sender) => move |button| {
                                        send!(sender, SlaveParameterTunerMsg::SetPropellerPwmFreqCalibration(button.value()));
                                    }
                                },
                            },
                            add = &ActionRow {
                                set_child = Some(&Scale::with_range(Orientation::Horizontal, -0.1, 0.1, 0.0001)) {
                                    set_width_request: CARD_MIN_WIDTH,
                                    set_round_digits: 4,
                                    set_value: track!(model.changed(SlaveParameterTunerModel::propeller_pwm_frequency_calibration()), *model.get_propeller_pwm_frequency_calibration() as f64),
                                    connect_value_changed(sender) => move |scale| {
                                        send!(sender, SlaveParameterTunerMsg::SetPropellerPwmFreqCalibration(scale.value()));
                                    }
                                }
                            },
                        },
                    },
                },
                add: group_propeller = &PreferencesGroup {
                    set_title: "???????????????",
                    add = &FlowBox {
                        set_activate_on_single_click: false,
                        set_valign: Align::Start,
                        set_row_spacing: 12,
                        set_selection_mode: SelectionMode::None,
                        factory!(model.propellers)
                    },
                },
            },
            add = &PreferencesPage {
                set_title: "?????????",
                set_icon_name: Some("media-playlist-repeat-symbolic"),
                set_hexpand: true,
                set_vexpand: true,
                set_can_focus: false,
                add: group_pid = &PreferencesGroup {
                    set_title: "PID ??????",
                    add = &FlowBox {
                        set_activate_on_single_click: false,
                        set_valign: Align::Start,
                        set_row_spacing: 12,
                        set_selection_mode: SelectionMode::None,
                        factory!(model.control_loops)
                    },
                },
            },
            set_title: {
                Some("????????????")
            },
            set_destroy: track!(model.changed(SlaveParameterTunerModel::stopped()), *model.get_stopped()),
            connect_close_request(sender) => move |_window| {
                send!(sender, SlaveParameterTunerMsg::StopDebug);
                Inhibit(false)
            },
        }
    }
    fn post_init() {
        let groups = [&group_propeller, &group_pid];
        let clamps = groups.iter().map(|x| x.parent().and_then(|x| x.parent()).and_then(|x| x.dynamic_cast::<Clamp>().ok())).filter_map(|x| x);
        for clamp in clamps {
            clamp.set_maximum_size(10000);
        }
        let overlay: ToastOverlay = window.content().unwrap().dynamic_cast().unwrap();
        let leaflet: Leaflet = overlay.child().unwrap().dynamic_cast().unwrap();
        let root_box: GtkBox = leaflet.observe_children().into_iter().find_map(|x| x.dynamic_cast().ok()).unwrap();
        let header_bar: HeaderBar = root_box.first_child().unwrap().dynamic_cast().unwrap();
        relm4_macros::view! {
            HeaderBar::from(header_bar) {
                pack_start = &Button {
                    set_css_classes: &["suggested-action"],
                    set_halign: Align::Center,
                    set_child = Some(&GtkBox) {
                        set_spacing: 6,
                        append = &Image {
                            set_icon_name: Some("document-save-symbolic"),
                        },
                        append = &Label {
                            set_label: "??????",
                        },
                    },
                    connect_clicked(sender) => move |_button| {
                        send!(sender, SlaveParameterTunerMsg::ApplyParameters);
                    },
                },
                pack_end = &Button {
                    set_css_classes: &["destructive-action"],
                    set_halign: Align::Center,
                    set_child = Some(&GtkBox) {
                        set_spacing: 6,
                        append = &Image {
                            set_icon_name: Some("view-refresh-symbolic"),
                        },
                        append = &Label {
                            set_label: "??????",
                        },
                    },
                    connect_clicked(sender) => move |_button| {
                        send!(sender, SlaveParameterTunerMsg::ResetParameters);
                    },
                },
            }
        }
    }
}

impl Debug for SlaveParameterTunerWidgets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.root_widget().fmt(f)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
struct SlaveParameterTunerLoadPacket {
    load_parameters: ()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
struct SlaveParameterTunerSavePacket {
    save_parameters: ()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct SlaveParameterTunerSetPropellerPacket {
    set_propeller_values: HashMap<String, i8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct SlaveParameterTunerSetControlLoopPacket {
    set_control_loop_parameters: HashMap<String, ControlLoop>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct SlaveParameterTunerSetDebugModeEnabledPacket {
    set_debug_mode_enabled: bool,
}


#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SlaveParameterTunerPacket {
    set_propeller_pwm_freq_calibration: f64,
    set_propeller_parameters: HashMap<String, Propeller>,
    set_control_loop_parameters: HashMap<String, ControlLoop>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SlaveParameterTunerFeedbackPacket {
    feedbacks: SlaveParameterTunerFeedbackValuePacket,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SlaveParameterTunerFeedbackValuePacket {
    control_loops: HashMap<String, f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct SlaveParameterTunerUpdatePacket {
    update_parameters: ()
}

#[derive(Debug)]
enum SlaveParameterTunerTcpMsg {
    UploadParameters(SlaveParameterTunerPacket),
    RequestParameters,
    SetDebugModeEnabled(bool),
    PreviewPropeller(String, i8),
    PreviewPropellers(HashMap<String, i8>),
    PreviewControlLoop(String, ControlLoop),
    PreviewControlLoops(HashMap<String, ControlLoop>),
    ConnectionLost(IOError),
    Terminate,
}

async fn parameter_tuner_handler(mut tcp_stream: TcpStream,
                                 tcp_sender: async_std::channel::Sender<SlaveParameterTunerTcpMsg>,
                                 tcp_receiver: async_std::channel::Receiver<SlaveParameterTunerTcpMsg>,
                                 model_sender: Sender<SlaveParameterTunerMsg>) -> Result<(), IOError> {
    fn current_millis() -> u128 {
        SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_millis()
    }
    const PREVIEW_TIME_MILLIS: u128 = 1000;
    let last_propeller_preview_timestamp = async_std::sync::Arc::new(async_std::sync::Mutex::new(None as Option<u128>));
    let preview_propellers_value = async_std::sync::Arc::new(async_std::sync::Mutex::new(HashMap::<String, i8>::new()));
    let preview_control_loops = async_std::sync::Arc::new(async_std::sync::Mutex::new(HashMap::<String, ControlLoop>::new()));
    let receive_task = task::spawn(clone!(@strong tcp_stream, @strong model_sender, @strong tcp_sender => async move {
        let mut tcp_stream = tcp_stream.clone();
        let mut buf = [0u8; 1024];
        tcp_sender.try_send(SlaveParameterTunerTcpMsg::RequestParameters).unwrap_or(());
        loop {
            buf.fill(0);
            if let Err(err) = tcp_stream.read(&mut buf).await {
                tcp_sender.send(SlaveParameterTunerTcpMsg::ConnectionLost(err)).await.unwrap_or_default();
                break;
            } else {
                let json_string = match std::str::from_utf8(buf.split(|x| x.eq(&0)).next().unwrap()) {
                    Ok(string) => string,
                    Err(_) => continue,
                };
                if json_string.is_empty() {
                    tcp_sender.send(SlaveParameterTunerTcpMsg::ConnectionLost(IOError::new(std::io::ErrorKind::ConnectionAborted, "??????????????????????????????EOF???"))).await.unwrap_or_default();
                    break;
                }
                let msg = serde_json::from_str::<SlaveParameterTunerFeedbackPacket>(&json_string).map(SlaveParameterTunerMsg::FeedbacksReceived)
                    .or_else(|_| serde_json::from_str::<SlaveParameterTunerPacket>(&json_string).map(SlaveParameterTunerMsg::ParametersReceived));
                match msg {
                    Ok(msg @ SlaveParameterTunerMsg::FeedbacksReceived(_)) => {
                        send!(model_sender, msg);
                    },
                    Ok(msg @ SlaveParameterTunerMsg::ParametersReceived(_)) => {
                        send!(model_sender, msg);
                    },
                    Ok(_) => unreachable!(),
                    Err(err) => eprintln!("?????????????????????????????????JSON????????????{}?????????{}???", err.to_string(), json_string),
                }
            }
        }
    }));

    let parameter_preview_task = task::spawn(clone!(@strong tcp_sender, @strong preview_propellers_value, @strong preview_control_loops => async move {
        loop {
            if !preview_propellers_value.lock().await.is_empty() {
                let propeller_values = std::mem::replace(&mut *preview_propellers_value.lock().await, HashMap::new());
                if tcp_sender.send(SlaveParameterTunerTcpMsg::PreviewPropellers(propeller_values)).await.is_err() {
                    break;
                }
            }
            if !preview_control_loops.lock().await.is_empty() {
                let control_loops = std::mem::replace(&mut *preview_control_loops.lock().await, HashMap::new());
                if tcp_sender.send(SlaveParameterTunerTcpMsg::PreviewControlLoops(control_loops)).await.is_err() {
                    break;
                }
            }
            task::sleep(Duration::from_millis(100)).await;
            
        }
    }));
    
    let stop_propeller_preview_task = task::spawn(clone!(@strong tcp_sender, @strong last_propeller_preview_timestamp => async move {
        loop {
            let mut last_millis = last_propeller_preview_timestamp.lock().await;
            if let Some(millis) = *last_millis {
                if current_millis() - millis >= PREVIEW_TIME_MILLIS {
                    if tcp_sender.send(SlaveParameterTunerTcpMsg::PreviewPropellers(DEFAULT_PROPELLERS.iter().map(|x| (x.to_string(), 0i8)).collect())).await.is_err() {
                        break;
                    }
                    *last_millis = None;
                }
            }
            drop(last_millis);        // ?????????????????????
            task::sleep(Duration::from_millis(500)).await;
        }
    }));
    
    loop {
        match tcp_receiver.recv().await {
            Ok(msg) => {
                match msg {
                    SlaveParameterTunerTcpMsg::UploadParameters(parameters) => {
                        let json_string = serde_json::to_string(&parameters).unwrap();
                        tcp_stream.write_all(json_string.as_bytes()).await?;
                        tcp_stream.flush().await?;
                        let json_string = serde_json::to_string(&SlaveParameterTunerSavePacket::default()).unwrap();
                        tcp_stream.write_all(json_string.as_bytes()).await.unwrap_or_default();
                        tcp_stream.flush().await?;
                    },
                    SlaveParameterTunerTcpMsg::RequestParameters => {
                        let json_string = serde_json::to_string(&SlaveParameterTunerLoadPacket::default()).unwrap();
                        tcp_stream.write_all(json_string.as_bytes()).await?;
                        tcp_stream.flush().await?;
                    },
                    SlaveParameterTunerTcpMsg::Terminate => {
                        receive_task.cancel().await;
                        parameter_preview_task.cancel().await;
                        stop_propeller_preview_task.cancel().await;
                        break;
                    },
                    SlaveParameterTunerTcpMsg::ConnectionLost(err) => {
                        send!(model_sender, SlaveParameterTunerMsg::StopDebug);
                        tcp_stream.shutdown(std::net::Shutdown::Both).unwrap_or_default();
                        tcp_receiver.close();
                        return Err(err);
                    },
                    SlaveParameterTunerTcpMsg::SetDebugModeEnabled(enabled) => {
                        let json_string = serde_json::to_string(&SlaveParameterTunerSetDebugModeEnabledPacket {
                            set_debug_mode_enabled: enabled,
                        }).unwrap();
                        tcp_stream.write_all(json_string.as_bytes()).await?;
                        tcp_stream.flush().await?;
                    },
                    SlaveParameterTunerTcpMsg::PreviewPropeller(name, value) => {
                        preview_propellers_value.lock().await.insert(name, value);
                        *last_propeller_preview_timestamp.lock().await = Some(current_millis());
                    },
                    SlaveParameterTunerTcpMsg::PreviewPropellers(propeller_values) => {
                        let json_string = serde_json::to_string(&SlaveParameterTunerSetPropellerPacket {
                            set_propeller_values: propeller_values,
                        }).unwrap();
                        tcp_stream.write_all(json_string.as_bytes()).await?;
                        tcp_stream.flush().await?;
                    },
                    SlaveParameterTunerTcpMsg::PreviewControlLoops(control_loops) => {
                        let json_string = serde_json::to_string(&SlaveParameterTunerSetControlLoopPacket {
                            set_control_loop_parameters: control_loops,
                        }).unwrap();
                        tcp_stream.write_all(json_string.as_bytes()).await?;
                        tcp_stream.flush().await?;
                    },
                    SlaveParameterTunerTcpMsg::PreviewControlLoop(name, value) => {
                        preview_control_loops.lock().await.insert(name, value);
                    },
                }
            },
            Err(_) => (),
        }
    }
    tcp_receiver.close();
    Ok(())
}

impl MicroModel for SlaveParameterTunerModel {
    type Msg = SlaveParameterTunerMsg;
    type Widgets = SlaveParameterTunerWidgets;
    type Data = Sender<SlaveMsg>;
    
    fn update(&mut self, msg: SlaveParameterTunerMsg, parent_sender: &Sender<SlaveMsg>, sender: Sender<SlaveParameterTunerMsg>) {
        self.reset();
        
        match msg {
            SlaveParameterTunerMsg::SetPropellerLowerDeadzone(index, value) => {
                if let Some(propeller) = self.propellers.get_mut(index) {
                    propeller.reset();
                    propeller.set_deadzone_lower(value);
                    propeller.set_deadzone_upper(max(*propeller.get_deadzone_upper(), value));
                }               // ????????? unsafe ???????????????????????????????????????????????????????????????????????????????????????
                if let (Some(propeller), Some(msg_sender)) = (self.propellers.get(index), self.get_tcp_msg_sender()) {
                    msg_sender.try_send(SlaveParameterTunerTcpMsg::PreviewPropeller(propeller.get_key().clone(), value)).unwrap_or(());
                }
            },
            SlaveParameterTunerMsg::SetPropellerUpperDeadzone(index, value) => {
                if let Some(propeller) = self.propellers.get_mut(index) {
                    propeller.reset();
                    propeller.set_deadzone_upper(value);
                    propeller.set_deadzone_lower(min(*propeller.get_deadzone_lower(), value));
                }
                if let (Some(propeller), Some(msg_sender)) = (self.propellers.get(index), self.get_tcp_msg_sender()) {
                    msg_sender.try_send(SlaveParameterTunerTcpMsg::PreviewPropeller(propeller.get_key().clone(), value)).unwrap_or(());
                }
            },
            SlaveParameterTunerMsg::SetPropellerPowerPositive(index, value) => {
                if let Some(propeller) = self.propellers.get_mut(index) {
                    propeller.reset();
                    propeller.set_power_positive(value);
                }
            },
            SlaveParameterTunerMsg::SetPropellerPowerNegative(index, value) => {
                if let Some(propeller) = self.propellers.get_mut(index) {
                    propeller.reset();
                    propeller.set_power_negative(value);
                }
            },
            SlaveParameterTunerMsg::SetPropellerReversed(index, reversed) => {
                if let Some(propeller) = self.propellers.get_mut(index) {
                    propeller.reset();
                    propeller.set_reversed(reversed);
                }
            },
            SlaveParameterTunerMsg::SetPropellerEnabled(index, enabled) => {
                if let Some(propeller) = self.propellers.get_mut(index) {
                    propeller.reset();
                    propeller.set_enabled(enabled);
                }
            },
            SlaveParameterTunerMsg::SetP(index, value) => {
                if let Some(pids) = self.control_loops.get_mut(index) {
                    pids.reset();
                    pids.set_p(value);
                }
                if let (Some(pids), Some(msg_sender)) = (self.control_loops.get(index), self.get_tcp_msg_sender()) {
                    msg_sender.try_send(SlaveParameterTunerTcpMsg::PreviewControlLoop.apply(pids.to_control_loop())).unwrap_or(());
                }
            },
            SlaveParameterTunerMsg::SetI(index, value) => {
                if let Some(pids) = self.control_loops.get_mut(index) {
                    pids.reset();
                    pids.set_i(value);
                }
                if let (Some(pids), Some(msg_sender)) = (self.control_loops.get(index), self.get_tcp_msg_sender()) {
                    msg_sender.try_send(SlaveParameterTunerTcpMsg::PreviewControlLoop.apply(pids.to_control_loop())).unwrap_or(());
                }
            },
            SlaveParameterTunerMsg::SetD(index, value) => {
                if let Some(pids) = self.control_loops.get_mut(index) {
                    pids.reset();
                    pids.set_d(value);
                }
                if let (Some(pids), Some(msg_sender)) = (self.control_loops.get(index), self.get_tcp_msg_sender()) {
                    msg_sender.try_send(SlaveParameterTunerTcpMsg::PreviewControlLoop.apply(pids.to_control_loop())).unwrap_or(());
                }
            },
            SlaveParameterTunerMsg::ResetParameters => {
                if let Some(msg_sender) = self.get_tcp_msg_sender() {
                    msg_sender.try_send(SlaveParameterTunerTcpMsg::RequestParameters).unwrap_or(());
                }
            },
            SlaveParameterTunerMsg::ApplyParameters => {
                if let Some(msg_sender) = self.get_tcp_msg_sender() {
                    msg_sender.try_send(SlaveParameterTunerTcpMsg::UploadParameters(SlaveParameterTunerPacket {
                        set_propeller_pwm_freq_calibration: self.propeller_pwm_frequency_calibration,
                        set_propeller_parameters: PropellerModel::vec_to_map(self.propellers.iter().collect()),
                        set_control_loop_parameters: ControlLoopModel::vec_to_map(self.control_loops.iter().collect()),
                    })).unwrap_or(());
                    
                }
            },
            SlaveParameterTunerMsg::StartDebug(tcp_stream) => {
                let (tcp_sender, tcp_receiver) = async_std::channel::bounded::<SlaveParameterTunerTcpMsg>(128);
                self.tcp_msg_sender = Some(tcp_sender.clone());
                let sender = sender.clone();
                tcp_sender.try_send(SlaveParameterTunerTcpMsg::SetDebugModeEnabled(true)).unwrap_or(());
                let handle = task::spawn(parameter_tuner_handler(tcp_stream, tcp_sender, tcp_receiver, sender));
                send!(parent_sender, SlaveMsg::TcpMessage(SlaveTcpMsg::Block(handle)));
            },
            SlaveParameterTunerMsg::StopDebug => {
                if let Some(msg_sender) = self.get_tcp_msg_sender() {
                    msg_sender.try_send(SlaveParameterTunerTcpMsg::SetDebugModeEnabled(false)).unwrap_or(());
                    msg_sender.try_send(SlaveParameterTunerTcpMsg::Terminate).unwrap_or_default();
                    self.set_tcp_msg_sender(None);
                    self.set_stopped(true);
                }
            },
            SlaveParameterTunerMsg::FeedbacksReceived(SlaveParameterTunerFeedbackPacket { feedbacks: SlaveParameterTunerFeedbackValuePacket { control_loops } }) => {
                let limit = *self.get_graph_view_point_num_limit() as usize;
                for index in 0..self.control_loops.len() {
                    let control_loop_model = self.control_loops.get_mut(index).unwrap();
                    if let Some(&control_loop_value) = control_loops.get(control_loop_model.get_key()) {
                        let feedbacks = control_loop_model.get_mut_feedbacks();
                        if feedbacks.len() == limit {
                            feedbacks.pop_front();
                        }
                        feedbacks.push_back(control_loop_value);
                    }
                }
            },
            SlaveParameterTunerMsg::ParametersReceived(SlaveParameterTunerPacket { set_propeller_pwm_freq_calibration: pwm_freq_calibration, set_propeller_parameters: propellers, set_control_loop_parameters: control_loops }) => {
                self.set_propeller_pwm_frequency_calibration(pwm_freq_calibration);
                for index in 0..self.propellers.len() {
                    let propeller_model = self.propellers.get_mut(index).unwrap();
                    if let Some(propeller) = propellers.get(propeller_model.get_key()) {
                        propeller_model.set_deadzone_lower(propeller.deadzone_lower.min(propeller.deadzone_upper));
                        propeller_model.set_deadzone_upper(propeller.deadzone_upper.max(propeller.deadzone_lower));
                        propeller_model.set_power_positive(propeller.power_positive);
                        propeller_model.set_power_negative(propeller.power_negative);
                        propeller_model.set_reversed(propeller.reversed);
                        propeller_model.set_enabled(propeller.enabled);
                    }
                }
                for index in 0..self.control_loops.len() {
                    let control_loop_model = self.control_loops.get_mut(index).unwrap();
                    if let Some(control_loop) = control_loops.get(control_loop_model.get_key()) {
                        control_loop_model.set_p(control_loop.p);
                        control_loop_model.set_i(control_loop.i);
                        control_loop_model.set_d(control_loop.d);
                    }
                }
            },
            SlaveParameterTunerMsg::SetPropellerPwmFreqCalibration(cal) => {
              self.set_propeller_pwm_frequency_calibration(cal);
            },
        }
    }
}
