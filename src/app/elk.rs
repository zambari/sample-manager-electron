//! The Elk Sample Order mode: device requests and their answers.

use super::*;
use crate::device::{ElkCmd, ElkEv};
use crate::ui::elk_view::{self, ElkIntent};

impl App {
    pub(super) fn enter_elk(&mut self) {
        self.elk_mode = true;
        self.editor_focused = false;
        if !self.elk.asked_projects {
            self.elk.asked_projects = true;
            self.device.elk(ElkCmd::ListProjects);
        }
    }

    pub(super) fn show_elk(&mut self, ui: &mut egui::Ui) {
        for i in elk_view::show(ui, &mut self.elk) {
            match i {
                ElkIntent::BackToEditor => self.elk_mode = false,
                ElkIntent::ListProjects => {
                    self.elk.projects = None;
                    self.device.elk(ElkCmd::ListProjects);
                }
                ElkIntent::Read => self.elk_read(),
                ElkIntent::Push => {
                    let Some(p) = &self.elk.project else { continue };
                    let msgs = p.build_push(&self.elk.order);
                    let settings = p.settings_data().to_vec();
                    self.elk.expect = Some(self.elk.new_pool());
                    self.elk.busy = Some(("Sending to device".into(), 0, msgs.len()));
                    self.device.elk(ElkCmd::Push { msgs, settings });
                }
            }
        }
    }

    fn elk_read(&mut self) {
        self.elk.busy = Some(("Reading project".into(), 0, 129));
        self.device.elk(ElkCmd::Read);
    }

    pub(super) fn elk_event(&mut self, e: ElkEv) {
        let st = &mut self.elk;
        match e {
            ElkEv::Projects(r) => st.projects = Some(r),
            ElkEv::Progress { label, done, total } => st.busy = Some((label, done, total)),
            ElkEv::Loaded(r) => {
                st.busy = None;
                match r {
                    Ok(p) => {
                        let used = p.pool.iter().skip(1).filter(|e| !e.is_empty()).count();
                        // The device may renumber inodes; compare the files.
                        let differ = |want: &[crate::device::PoolEntry]| {
                            want.iter()
                                .zip(&p.pool)
                                .filter(|(a, b)| a.is_empty() != b.is_empty() || a.key() != b.key())
                                .count()
                        };
                        st.status = match st.expect.take() {
                            Some(want) if differ(&want) == 0 => {
                                "Pushed and read back: the device has the new order. Save on the device (PROJECT › SAVE) to keep it.".to_owned()
                            }
                            Some(want) => {
                                let off = differ(&want);
                                let msg = format!("Read back after push: {off} slots differ from what was sent. Don't save; reload the project from +Drive.");
                                self.errors.push(msg.clone());
                                msg
                            }
                            None => format!("Read {used} samples"),
                        };
                        st.set_project(p);
                    }
                    Err(e) => {
                        st.expect = None;
                        st.status = format!("Read failed: {e}");
                    }
                }
            }
            ElkEv::Names(v) => st.names.extend(v),
            ElkEv::Pushed(r) => match r {
                Ok(n) => {
                    st.status = format!("Sent {n} messages; reading back to check…");
                    self.elk_read();
                }
                Err(e) => {
                    st.busy = None;
                    st.expect = None;
                    st.status = format!("Push failed: {e}");
                    self.errors.push(format!("push to device: {e}"));
                }
            },
        }
    }
}
