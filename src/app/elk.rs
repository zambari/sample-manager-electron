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
                ElkIntent::Local(k, open) => {
                    let Some(l) = self.links.local(k.0, k.1) else {
                        self.elk.status = "That local copy was changed or moved since the transfer, so it no longer matches the device file.".into();
                        continue;
                    };
                    let (local, note) = (l.local.clone(), super::editing::link_note(l));
                    if open {
                        self.elk_mode = false;
                        self.load_into_editor(&local);
                    } else {
                        self.preview_toggle(&local);
                    }
                    self.status = note;
                }
                ElkIntent::Push => {
                    let Some(p) = &self.elk.project else { continue };
                    let msgs = p.build_push(&self.elk.order);
                    if msgs.is_empty() {
                        self.elk.status = "Nothing to send: no pattern, kit or sound changes with this order.".into();
                        continue;
                    }
                    crate::log::line(format!(
                        "elk: partial push, {} of {} dump messages changed",
                        msgs.len(),
                        p.msg_count()
                    ));
                    let settings = p.settings_data().to_vec();
                    self.elk.expect = Some(self.elk.new_pool());
                    self.elk.busy = Some(("Sending to device".into(), 0, msgs.len()));
                    self.device.elk(ElkCmd::Push { msgs, settings });
                }
            }
        }
    }

    /// Which pool files we have a local copy of, for the view.
    pub(super) fn refresh_elk_links(&mut self) {
        let Some(p) = &self.elk.project else { return };
        let linked: std::collections::HashMap<_, _> = p
            .pool
            .iter()
            .filter(|e| !e.is_empty())
            .filter_map(|e| {
                let k = e.key();
                let l = self.links.known(k.0, k.1)?;
                Some((k, (l.device_name().to_owned(), l.local.clone(), l.uploaded)))
            })
            .collect();
        if linked.len() != self.elk.linked.len() {
            crate::log::line(format!("elk: {} pool files have a local copy", linked.len()));
        }
        self.elk.linked = linked;
    }

    fn elk_read(&mut self) {
        self.elk.busy = Some(("Reading project".into(), 0, 129));
        let known = self.elk.names.keys().copied().collect();
        self.device.elk(ElkCmd::Read { known });
    }

    pub(super) fn elk_event(&mut self, e: ElkEv) {
        let st = &mut self.elk;
        match e {
            ElkEv::Projects(r) => st.projects = Some(r),
            ElkEv::Partial(p) => {
                st.set_project(p);
                self.refresh_elk_links();
            }
            ElkEv::Uses(v) => {
                if let Some(p) = st.project.as_mut().filter(|p| p.partial && p.uses.len() == v.len()) {
                    p.uses = v;
                }
            }
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
                        self.refresh_elk_links();
                    }
                    Err(e) => {
                        st.expect = None;
                        st.status = format!("Read failed: {e}");
                        // A pool shown early stays, but can't be pushed.
                        if let Some(p) = st.project.as_mut().filter(|p| p.partial) {
                            p.read_only = Some(format!("the read didn't finish ({e}); load again to push"));
                        }
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
