/* firmware_update.rs
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

use std::{path::PathBuf, fmt::Debug};
use async_std::{io::ReadExt, net::TcpStream, task, prelude::*};

use glib::Sender;
use glib_macros::clone;
use gtk::{Align, Box as GtkBox, Orientation, prelude::*, FileFilter, ProgressBar, FileChooserAction, Button};
use adw::{HeaderBar, PreferencesGroup, StatusPage, Window, prelude::*, ActionRow, Carousel};
use once_cell::unsync::OnceCell;
use relm4::{send, MicroWidgets, MicroModel};
use relm4_macros::micro_widget;

use serde::{Serialize, Deserialize};
use derivative::*;

use crate::prelude::*;
use crate::slave::SlaveTcpMsg;
use crate::ui::generic::select_path;

use super::SlaveMsg;

pub enum SlaveFirmwareUpdaterMsg {
    StartUpload,
    NextStep,
    FirmwareFileSelected(PathBuf),
    FirmwareUploadProgressUpdated(f32),
    FirmwareUploadFailed,
}

#[tracker::track(pub)]
#[derive(Debug, Derivative)]
#[derivative(Default)]
pub struct SlaveFirmwareUpdaterModel {
    current_page: u32,
    firmware_file_path: Option<PathBuf>,
    firmware_uploading_progress: f32,
    #[no_eq]
    _tcp_stream: OnceCell<TcpStream>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SlaveFirmwareUpdatePacket {
    firmware_update: SlaveFirmwarePacket,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SlaveFirmwarePacket {
    size: usize,
    compression: String,
    md5: String,
}

impl SlaveFirmwareUpdaterModel {
    pub fn new(tcp_stream: TcpStream) -> SlaveFirmwareUpdaterModel {
        SlaveFirmwareUpdaterModel {
            _tcp_stream: OnceCell::from(tcp_stream),
            ..Default::default()
        }
    }
    
    pub fn get_tcp_stream(&self) -> &TcpStream {
        self._tcp_stream.get().unwrap()
    }
}

impl MicroModel for SlaveFirmwareUpdaterModel {
    type Msg = SlaveFirmwareUpdaterMsg;
    type Widgets = SlaveFirmwareUpdaterWidgets;
    type Data = Sender<SlaveMsg>;
    
    fn update(&mut self, msg: SlaveFirmwareUpdaterMsg, parent_sender: &Sender<SlaveMsg>, sender: Sender<SlaveFirmwareUpdaterMsg>) {
        self.reset();
        match msg {
            SlaveFirmwareUpdaterMsg::NextStep => self.set_current_page(self.get_current_page().wrapping_add(1)),
            SlaveFirmwareUpdaterMsg::FirmwareFileSelected(path) => self.set_firmware_file_path(Some(path)),
            SlaveFirmwareUpdaterMsg::FirmwareUploadProgressUpdated(progress) => {
                self.set_firmware_uploading_progress(progress);
                if progress >= 1.0 || progress < 0.0 {
                    send!(sender, SlaveFirmwareUpdaterMsg::NextStep);
                }
            },
            SlaveFirmwareUpdaterMsg::StartUpload => {
                if let Some(path) = self.get_firmware_file_path() {
                    send!(sender, SlaveFirmwareUpdaterMsg::NextStep);
                    let mut tcp_stream = self.get_tcp_stream().clone();
                    let handle = task::spawn(clone!(@strong sender, @strong path => async move {
                        match async_std::fs::File::open(path).await {
                            Ok(mut file) => {
                                let mut bytes = Vec::new();
                                file.read_to_end(&mut bytes).await?;
                                let bytes = bytes.as_slice();
                                let md5_string = format!("{:x}", md5::compute(&bytes));
                                let packet = SlaveFirmwareUpdatePacket {
                                    firmware_update: SlaveFirmwarePacket {
                                        size: bytes.len(),
                                        compression: String::from("none"),
                                        md5: md5_string,
                                    }
                                };
                                let json = serde_json::to_string(&packet).unwrap();
                                let mut json_bytes = json.as_bytes();
                                async_std::io::copy(&mut json_bytes, &mut tcp_stream).await?;
                                let chunks = bytes.chunks(1024);
                                let chunk_num = chunks.len();
                                if chunk_num > 0 {
                                    for (chunk_index, chunk) in chunks.enumerate() {
                                        tcp_stream.write(chunk).await?;
                                        let progress = (chunk_index + 1) as f32 / chunk_num as f32;
                                        send!(sender, SlaveFirmwareUpdaterMsg::FirmwareUploadProgressUpdated(progress));
                                    }
                                    tcp_stream.flush().await?;
                                } else {
                                    send!(sender, SlaveFirmwareUpdaterMsg::FirmwareUploadProgressUpdated(1.0));
                                }
                                Ok(())
                            },
                            Err(err) => Err(err),
                        }
                    }));
                    let handle = task::spawn(async move {
                        let result = handle.await;
                        if result.is_err() {
                            send!(sender, SlaveFirmwareUpdaterMsg::FirmwareUploadFailed);
                        }
                        result
                    });
                    send!(parent_sender, SlaveMsg::TcpMessage(SlaveTcpMsg::Block(handle)));
                }
            },
            SlaveFirmwareUpdaterMsg::FirmwareUploadFailed => send!(sender, SlaveFirmwareUpdaterMsg::FirmwareUploadProgressUpdated(-1.0)),
        }
    }
}

#[micro_widget(pub)]
impl MicroWidgets<SlaveFirmwareUpdaterModel> for SlaveFirmwareUpdaterWidgets {
    view! {
        window = Window {
            set_title: Some("??????????????????"),
            set_width_request: 480,
            set_height_request: 480,
            set_destroy_with_parent: true,
            set_modal: true,
            set_content = Some(&GtkBox) {
                set_orientation: Orientation::Vertical,
                append = &HeaderBar {
                    set_sensitive: track!(model.changed(SlaveFirmwareUpdaterModel::firmware_uploading_progress()), *model.get_firmware_uploading_progress() <= 0.0 || *model.get_firmware_uploading_progress() >= 1.0),
                },
                append: carousel = &Carousel {
                    set_hexpand: true,
                    set_vexpand: true,
                    set_interactive: false,
                    scroll_to_page: track!(model.changed(SlaveFirmwareUpdaterModel::current_page()), model.current_page, true),
                    append = &StatusPage {
                        set_icon_name: Some("software-update-available-symbolic"),
                        set_title: "??????????????????????????????",
                        set_hexpand: true,
                        set_vexpand: true,
                        set_description: Some("???????????????????????????????????????????????????????????????"),
                        set_child = Some(&Button) {
                            set_css_classes: &["suggested-action", "pill"],
                            set_halign: Align::Center,
                            set_label: "?????????",
                            connect_clicked(sender) => move |_button| {
                                send!(sender, SlaveFirmwareUpdaterMsg::NextStep);
                            },
                        },
                    },
                    append = &StatusPage {
                        set_icon_name: Some("folder-open-symbolic"),
                        set_title: "?????????????????????",
                        set_hexpand: true,
                        set_vexpand: true,
                        set_description: Some("????????????????????????????????????????????????????????????"),
                        set_child = Some(&GtkBox) {
                            set_orientation: Orientation::Vertical,
                            set_spacing: 50,
                            append = &PreferencesGroup {
                                add = &ActionRow {
                                    set_title: "????????????",
                                    set_subtitle: track!(model.changed(SlaveFirmwareUpdaterModel::firmware_file_path()), &model.firmware_file_path.as_ref().map_or("???????????????".to_string(), |path| path.to_str().unwrap().to_string())),
                                    add_suffix: browse_firmware_file_button = &Button {
                                        set_label: "??????",
                                        set_valign: Align::Center,
                                        connect_clicked(sender, window) => move |_button| {
                                            let filter = FileFilter::new();
                                            filter.add_suffix("bin");
                                            filter.set_name(Some("????????????"));
                                            std::mem::forget(select_path(FileChooserAction::Open, &[filter], &window, clone!(@strong sender => move |path| {
                                                match path {
                                                    Some(path) => {
                                                        send!(sender, SlaveFirmwareUpdaterMsg::FirmwareFileSelected(path));
                                                    },
                                                    None => (),
                                                }
                                            }))); // ??????????????????
                                        },
                                    },
                                    set_activatable_widget: Some(&browse_firmware_file_button),
                                },
                            },
                            append = &Button {
                                set_css_classes: &["suggested-action", "pill"],
                                set_halign: Align::Center,
                                set_label: "????????????",
                                set_sensitive: track!(model.changed(SlaveFirmwareUpdaterModel::firmware_file_path()), model.get_firmware_file_path().as_ref().map_or(false, |pathbuf| pathbuf.exists() && pathbuf.is_file())),
                                connect_clicked(sender) => move |_button| {
                                    send!(sender, SlaveFirmwareUpdaterMsg::StartUpload);
                                },
                            }
                        },
                    },
                    append = &StatusPage {
                        set_icon_name: Some("folder-download-symbolic"),
                        set_title: "??????????????????...",
                        set_hexpand: true,
                        set_vexpand: true,
                        set_description: Some("?????????????????????????????????"),
                        set_child = Some(&GtkBox) {
                            set_orientation: Orientation::Vertical,
                            set_spacing: 50,
                            append = &ProgressBar {
                                set_fraction: track!(model.changed(SlaveFirmwareUpdaterModel::firmware_uploading_progress()), *model.get_firmware_uploading_progress() as f64)
                            },
                        },
                    },
                    append = &StatusPage {
                        set_icon_name: track!(model.changed(SlaveFirmwareUpdaterModel::firmware_uploading_progress()), if *model.get_firmware_uploading_progress() >= 0.0 { Some("emblem-ok-symbolic") } else { Some("dialog-warning-symbolic") }),
                        set_title: track!(model.changed(SlaveFirmwareUpdaterModel::firmware_uploading_progress()), if *model.get_firmware_uploading_progress() >= 0.0 { "??????????????????" } else { "??????????????????" }),
                        set_hexpand: true,
                        set_vexpand: true,
                        set_description: track!(model.changed(SlaveFirmwareUpdaterModel::firmware_uploading_progress()), Some(if *model.get_firmware_uploading_progress() >= 0.0 { "?????????????????????????????????????????????????????????" } else { "?????????????????????????????????????????????" })),
                        set_child = Some(&Button) {
                            set_css_classes: &["suggested-action", "pill"],
                            set_halign: Align::Center,
                            set_label: "??????",
                            connect_clicked(window) => move |_button| {
                                window.destroy();
                            },
                        },
                    },
                },
            },
        }
    }
}

impl Debug for SlaveFirmwareUpdaterWidgets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.root_widget().fmt(f)
    }
}
