//! Pending modal answers in frame order, including save and close follow-ups.

use super::*;

impl App {
    /// Run dialogs in their original order: one answer may open a later
    /// dialog during this same frame.
    pub(super) fn show_pending_dialogs(&mut self, ctx: &egui::Context) {
        self.show_transfer_dialogs(ctx);
        self.show_device_dialogs(ctx);
        self.show_file_removal_dialogs(ctx);
        self.show_kit_name_dialog(ctx);
        self.show_leave_dialog(ctx);
        self.show_save_dialogs(ctx);
        self.finish_pending_close(ctx);
    }

    fn show_transfer_dialogs(&mut self, ctx: &egui::Context) {
        // Modal last, so it paints over both panes.
        if let Some(p) = &self.pending {
            // A device move is a rename, which can't merge folders.
            let can_merge = matches!(
                p.what,
                PlanKind::Transfer
                    | PlanKind::Commit { .. }
                    | PlanKind::Device {
                        dir: device::Dir::Upload | device::Dir::Download | device::Dir::Copy,
                        ..
                    }
            ) && p.plan.ops.iter().any(|o| o.conflict && o.is_dir);
            if let Some(decision) = dialogs::conflicts(ctx, &p.plan, can_merge) {
                let pending = self.pending.take().expect("checked above");
                match decision {
                    Decision::Proceed(policy) => self.launch(&pending.plan, pending.what, policy),
                    Decision::Merge if matches!(pending.what, PlanKind::Device { .. }) => {
                        self.launch(&pending.plan, pending.what, Policy::Merge)
                    }
                    Decision::Merge => self.launch_merge(&pending.plan, pending.what),
                    Decision::Cancel => self.status = "Transfer cancelled".to_owned(),
                }
            }
        }

        if let Some(r) = &self.local_rename_pending {
            if let Some(yes) = dialogs::rename_local(ctx, &r.link.local, &r.to) {
                let r = self.local_rename_pending.take().expect("checked above");
                if yes {
                    self.rename_local_copy(r);
                }
            }
        }

        if let Some(d) = &self.slot_download_pending {
            let have: Vec<&Path> = d.have.iter().map(|(_, l)| l.as_path()).collect();
            if let Some(choice) = dialogs::slot_download(ctx, &have, d.paths.len(), d.role.label()) {
                let d = self.slot_download_pending.take().expect("checked above");
                self.answer_slot_download(d, choice);
            }
        }

        if let Some(p) = &self.slot_prefix_pending {
            let mut srcs: Vec<&PathBuf> = p.paths.iter().collect();
            srcs.sort_by_key(|s| p.labels.get(*s));
            let examples: Vec<String> = srcs
                .iter()
                .filter_map(|s| {
                    let name = device::wav_name(&s.file_name()?.to_string_lossy());
                    Some(pane_view::slot_prefixed(p.labels.get(*s)?, &name))
                })
                .collect();
            if let Some(choice) = dialogs::slot_prefix(ctx, &examples, p.paths.len()) {
                let p = self.slot_prefix_pending.take().expect("checked above");
                self.answer_slot_prefix(p, choice);
            }
        }
    }

    fn show_device_dialogs(&mut self, ctx: &egui::Context) {
        if let Some(n) = self.reconnect_ask {
            if let Some(read) = dialogs::device_reconnected(ctx, n) {
                self.reconnect_ask = None;
                self.answer_reconnect(read);
            }
        }

        if self.factory_popup && dialogs::factory_locked(ctx) {
            self.factory_popup = false;
        }

        if let Some((path, bytes)) = &self.device_load_pending {
            if let Some(yes) = dialogs::confirm_device_load(ctx, path, *bytes) {
                let (path, bytes) = self.device_load_pending.take().expect("checked above");
                if yes {
                    self.start_device_load(path, bytes);
                } else {
                    self.status = "Load cancelled".to_owned();
                }
            }
        }
    }

    fn show_file_removal_dialogs(&mut self, ctx: &egui::Context) {
        if let Some(groups) = &self.dupes_pending {
            if let Some(yes) = dialogs::confirm_duplicates(ctx, groups) {
                let groups = self.dupes_pending.take().expect("checked above");
                if yes {
                    let paths: Vec<PathBuf> = groups.into_iter().flat_map(|g| g.remove).collect();
                    self.status = format!("Moving {} duplicate(s) to Trash…", paths.len());
                    self.trash_local(paths);
                } else {
                    self.status = "Duplicate removal cancelled".to_owned();
                }
            }
        }

        if let Some(paths) = &self.delete_pending {
            if let Some(yes) = dialogs::confirm_trash(ctx, paths, true, true) {
                let paths = self.delete_pending.take().expect("checked above");
                if yes {
                    let n = paths.len();
                    self.run_device(device::Task::Delete { paths }, "Deleting", n, 0);
                } else {
                    self.status = "Delete cancelled".to_owned();
                }
            }
        }
    }

    fn show_kit_name_dialog(&mut self, ctx: &egui::Context) {
        if let Some(n) = &mut self.kit_naming {
            if let Some(ok) = dialogs::name_kit(ctx, &mut n.name, &n.dir, &mut n.grid) {
                let n = self.kit_naming.take().expect("checked above");
                if ok {
                    self.start_kit(n);
                } else {
                    self.status = "No SliceKit started".to_owned();
                }
            }
        }
    }

    fn show_leave_dialog(&mut self, ctx: &egui::Context) {
        // Closing the window with unsaved audio edits: ask first.
        let close = ctx.input(|i| i.viewport().close_requested());
        if close && self.any_dirty() && !self.allow_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if self.leave_pending.is_none() && self.leave_after_save.is_none() {
                // Ask about whichever slot has the unsaved edits.
                self.front_dirty();
                self.leave_pending = Some(Leave::Quit);
            }
        }

        if let Some(leave) = self.leave_pending.clone() {
            let name = self
                .editor
                .path()
                .and_then(|p| p.file_name())
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let tab = self.pane(Role::Dest).active_tab();
            let dest = (!tab.loading).then(|| {
                let d = tab.cwd.file_name().map_or_else(
                    || tab.cwd.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                );
                if tab.device {
                    format!("{d} on the device")
                } else {
                    d
                }
            });
            let why = match leave {
                Leave::Quit => dialogs::LeaveWhy::Quit,
                Leave::CloseKit => dialogs::LeaveWhy::CloseKit,
                _ => dialogs::LeaveWhy::Open,
            };
            if let Some(c) = dialogs::leave_editor(ctx, &name, why, dest.as_deref()) {
                self.leave_pending = None;
                self.answer_leave(leave, c);
            }
        }
    }

    fn show_save_dialogs(&mut self, ctx: &egui::Context) {
        if let Some(c) = &self.save_confirm {
            let (title, lines, ok) = c.text();
            if let Some(yes) = dialogs::confirm_overwrite(ctx, &title, &lines, &ok) {
                let c = self.save_confirm.take().expect("checked above");
                if yes {
                    self.confirmed_save(c);
                }
            }
        }

        if self.send_back.is_some() {
            let (taken, exists) = {
                let f = self.send_back.as_ref().expect("checked above");
                let name = f.name.trim();
                (
                    !name.is_empty() && self.device_has(&f.dir.join(name)),
                    f.local_dir.as_ref().is_some_and(|d| d.join(format!("{name}.wav")).exists()),
                )
            };
            let f = self.send_back.as_mut().expect("checked above");
            if let Some(ok) = dialogs::send_back(ctx, f, taken, exists) {
                let f = self.send_back.take().expect("checked above");
                if ok {
                    self.start_send_back(f);
                }
            }
        }
        if let Some(n) = &mut self.export_naming {
            if let Some(ok) = dialogs::name_export(ctx, &mut n.name, &n.dir, &n.taken) {
                let n = self.export_naming.take().expect("checked above");
                if ok {
                    let dst = n.dir.join(format!("{}.wav", n.name.trim()));
                    self.start_export(dst, n.mono);
                } else {
                    self.status = "Export cancelled".to_owned();
                }
            }
        }

        if self.save_scope_pending {
            let name = self
                .editor
                .path()
                .and_then(|p| p.file_name())
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let secs = |n: usize| self.editor.sample().map_or(0.0, |s| s.seconds(n));
            let sel = secs(self.editor.sel.1 - self.editor.sel.0);
            let whole = self.editor.file_seconds();
            if let Some(c) = dialogs::save_scope(ctx, &name, sel, whole) {
                self.save_scope_pending = false;
                match c {
                    SaveScope::Cancel => self.status = "Not saved".to_owned(),
                    SaveScope::Trim => self.save_in_place_as(false),
                    SaveScope::Whole => self.save_in_place_as(true),
                }
            }
        }
    }

    fn finish_pending_close(&mut self, ctx: &egui::Context) {
        // A save chosen in that dialog that never got going (its conflict or
        // mono question was cancelled, or everything was skipped): drop it.
        if self.leave_after_save.is_some()
            && !self.save_scope_pending
            && self.export_naming.is_none()
            && !self.editor.exporting
            && self.pending.is_none()
            && self.staged_upload.is_none()
        {
            self.leave_after_save = None;
        }

        if self.close_when_idle && self.active_jobs.is_empty() {
            self.close_when_idle = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}
