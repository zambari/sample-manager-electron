//! Device page headers, project/file controls, preview tools and loading/status chrome.

use super::*;

/// Height kept free under the slot panes for `memory_bar`.
pub(super) const MEM_BAR_H: f32 = 26.0;

/// The mode switch across the top of both full-window pages, with the
/// layout switches on the right.
pub(crate) fn mode_header(ui: &mut egui::Ui, st: &mut ElekState, out: &mut Vec<ElekIntent>) {
    let back = match &st.playing {
        Some(name) => ui
            .add(
                egui::Button::new(
                    RichText::new("▶ PLAYING").size(12.0).strong().color(theme::ON_ACCENT),
                )
                .fill(theme::ACCENT),
            )
            .on_hover_text(format!("Playing {name}\nClick to see it in the Sample Editor")),
        None => ui.add(theme::pane_title("Sample Editor", false)),
    };
    if back.clicked() {
        out.push(ElekIntent::BackToEditor);
    }
    ui.add_space(14.0);
    theme::device_group(ui, |ui| {
        if theme::device_switch(ui, true, true).clicked() {
            out.push(ElekIntent::ToggleDevice);
        }
        if crate::device::presence().available() {
            // The top tabs pick one place at a time: here a page is, so
            // +Drive isn't lit even when the right column shows it.
            match device_pages(ui, Some(st.page), false) {
                Some(DevicePick::Page(page)) => st.page = page,
                Some(DevicePick::Drive) => out.push(ElekIntent::ShowDrive),
                None => {}
            }
        }
    });
    ui.separator();
    if let Some(p) = &st.project {
        ui.label(RichText::new(format!("{} · {}", p.model.name(), p.versions)).weak().small());
        if let Some(f) = &p.from_file {
            let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            ui.label(RichText::new(format!("from file: {name}")).color(theme::DEVICE_ACCENT_BRIGHT).small())
                .on_hover_text(format!("{}\nNot on the device yet: Push sends the whole project into the device's memory.", f.display()));
        }
    }
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        // Only the Samples page is the pool, so only it can dock.
        if st.page == ElekPage::Order
            && ui
                .add(
                    egui::Button::new(RichText::new("⇥ Dock beside files").color(theme::ON_ACCENT))
                        .fill(theme::ACCENT),
                )
                .on_hover_text("Back to the editor, with the pool as one pane in the right column")
                .clicked()
        {
            out.push(ElekIntent::Dock);
        }
        ui.toggle_value(&mut st.hide_files, "Hide file panes")
            .on_hover_text("Give this view the whole window. Off: the file panes stay on the sides and this view takes the editor's place. Remembered.");
    });
}

/// Samples / Patterns / +Drive: the Device's sub-pages, drawn small right
/// after the Device switch. `active` is the page shown (`None` from the
/// editor, where neither is); `drive` is whether the right column shows
/// the +Drive. Returns what was clicked.
pub(crate) fn device_pages(ui: &mut egui::Ui, active: Option<ElekPage>, drive: bool) -> Option<DevicePick> {
    theme::device_tab_strip(
        ui,
        &[
            (DevicePick::Page(ElekPage::Order), "Samples", "The sample pool in two side-by-side panes"),
            (DevicePick::Page(ElekPage::Project), "Patterns", "Experimental: move, copy, clear and rename patterns (Digitakt II support is newer and less tested)"),
            (DevicePick::Page(ElekPage::Presets), "Presets", "Saved +Drive presets in two bank browsers"),
            (DevicePick::Page(ElekPage::Library), "Sounds", "The project's 128 pool sounds: rearrange, rename or delete them"),
            (DevicePick::Drive, "+Drive", "The device's file storage, in the right column"),
        ],
        |v| match v {
            DevicePick::Page(p) => active == Some(p),
            DevicePick::Drive => drive,
        },
    )
}

/// The full-window top: mode tabs, then the device bar with the page's own
/// tools and the status.
pub(crate) fn top(ui: &mut egui::Ui, st: &mut ElekState, out: &mut Vec<ElekIntent>, page_tools: impl FnOnce(&mut egui::Ui, &mut ElekState)) {
    ui.horizontal(|ui| mode_header(ui, st, out));
    ui.horizontal_wrapped(|ui| {
        device_bar(ui, st, out, true);
        if st.project.is_some() {
            ui.separator();
            page_tools(ui, st);
        }
        status_line(ui, st, true);
    });
    if st.project.is_none() {
        return;
    }
    // The page's own view options (the Samples / Patterns switch is up by Device).
    ui.add_space(2.0);
    ui.horizontal_wrapped(|ui| {
        match st.page {
            ElekPage::Order => preview_tools(ui, st, out),
            ElekPage::Project => crate::ui::elek_project_view::view_tools(ui, st),
            ElekPage::Presets => {},
            ElekPage::Library => crate::ui::elek_library_view::view_tools(ui, st),
        }
    });
    ui.separator();
}

/// How much of the device's sample memory the pool fills, as it will be
/// after a push (each slot's file size; uploads still on their way not
/// counted).
pub(super) fn memory_bar(ui: &mut egui::Ui, st: &ElekState) {
    let Some(p) = &st.project else { return };
    let total = p.model.sample_memory();
    let bytes = |pool: &mut dyn Iterator<Item = PoolEntry>| -> u64 {
        pool.filter(|e| !e.is_empty()).map(|e| e.size as u64).sum()
    };
    let now = bytes(&mut st.new_pool().into_iter());
    let read = bytes(&mut p.pool.iter().copied());
    let frac = now as f64 / total as f64;
    let fill = if frac > 1.0 {
        Color32::from_rgb(220, 70, 60)
    } else if frac > 0.9 {
        Color32::from_rgb(230, 150, 60)
    } else {
        theme::DEVICE_ACCENT
    };
    let mut text = format!("Sample memory {} / {} ({:.0}%)", mb(now), mb(total), frac * 100.0);
    if now != read {
        let d = now as i64 - read as i64;
        text += &format!("  ·  {}{} since read", if d >= 0 { "+" } else { "−" }, mb(d.unsigned_abs()));
    }
    if frac > 1.0 {
        text += "  ·  over the limit";
    }
    let waiting = st.filling.len();
    let mut tip = format!(
        "Sum of the file sizes in every filled slot, as the pool will be after a push. As read from the device: {}.",
        mb(read)
    );
    if waiting > 0 {
        tip += &format!(" {waiting} sample(s) still uploading aren't counted yet.");
    }
    // Device-font label over a thin square bar.
    ui.add_space(4.0);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), MEM_BAR_H - 6.0), Sense::hover());
    let painter = ui.painter();
    painter.text(rect.left_top(), egui::Align2::LEFT_TOP, text, oled_font(11.0), if frac > 0.9 { fill } else { OLED_DIM });
    let bar = egui::Rect::from_min_max(egui::pos2(rect.left(), rect.bottom() - 4.0), rect.right_bottom());
    painter.rect_filled(bar, 0.0, OLED_BG);
    let w = bar.width() * frac.min(1.0) as f32;
    painter.rect_filled(egui::Rect::from_min_size(bar.min, egui::vec2(w, bar.height())), 0.0, fill);
    resp.on_hover_text(tip);
}

/// Bytes as "12.3 MB".
fn mb(b: u64) -> String {
    format!("{:.1} MB", b as f64 / 1_048_576.0)
}

/// Preview toggle, background fetch and the cache size.
pub(super) fn preview_tools(ui: &mut egui::Ui, st: &mut ElekState, out: &mut Vec<ElekIntent>) {
    ui.toggle_value(&mut st.preview, "🔊 Preview on click")
        .on_hover_text("Clicking a slot, or moving onto it with ↑/↓, plays it. Without a local copy it is fetched from the device first (that slot jumps the fetch queue). Space plays the slot under the cursor either way.");
    match st.fetch {
        Some((done, total)) => {
            ui.add(theme::progress_bar(done as f32 / total.max(1) as f32).desired_width(140.0).text(format!("Fetching {done}/{total}")));
            if ui.small_button("Stop").on_hover_text("Stop fetching; what's already here stays").clicked() {
                out.push(ElekIntent::StopFetch);
            }
        }
        None => {
            if ui
                .button("⬇ Fetch all for preview")
                .on_hover_text("Copy every pool sample without a local copy into the preview cache, one at a time in the background. Other device work and clicked slots go first.")
                .clicked()
            {
                out.push(ElekIntent::FetchAll);
            }
        }
    }
    let (txt, files) = match st.cache {
        Some((b, f)) => (format!("Cache {}", mb(b)), f),
        None => ("Cache …".to_owned(), 0),
    };
    ui.menu_button(txt, |ui| {
        ui.label(format!("{files} sample(s) fetched for previewing, kept by content hash so they play instantly next time, whichever project uses them."));
        if ui
            .add_enabled(files > 0 && st.fetch.is_none(), egui::Button::new("Move cache to Trash"))
            .on_hover_text("Previews fetch again on demand")
            .clicked()
        {
            out.push(ElekIntent::PurgeCache);
            ui.close();
        }
    });
}

/// Load / reload, push, the +Drive project list, dump and close: what every
/// layout shares. `wide`: with words, else icons.
pub(crate) fn device_bar(ui: &mut egui::Ui, st: &mut ElekState, out: &mut Vec<ElekIntent>, wide: bool) {
    let busy = st.busy.is_some();
    if st.project.is_none() {
        // The full-window page shows a big load button of its own.
        if !wide && !busy {
            let b = egui::Button::new(RichText::new("⬇ Load current project").color(theme::DEVICE_ACCENT_BRIGHT));
            if ui.add(b).on_hover_text("Read the project open on the device now (from its memory)").clicked() {
                out.push(ElekIntent::Read);
            }
        }
        projects_menu(ui, st, out, wide);
        if !busy {
            file_menu(ui, st, out, wide);
        }
        return;
    }
    if ui.add_enabled(!busy, egui::Button::new(if wide { "⬇ Reload" } else { "⬇" })).on_hover_text("Read the project open on the device again (from its memory). Unpushed changes here are dropped.").clicked() {
        out.push(ElekIntent::Read);
    }
    let changed = st.changed();
    let waiting = st.filling.len();
    let from_file = st.project.as_ref().is_some_and(|p| p.from_file.is_some());
    // A stopped read: pool changes rewrite every pattern, so they need them all.
    let pool_blocked = !st.pool_pushable() && (st.pool_changed() > 0 || st.lib.sound_changed() > 0);
    let can_push = !busy && (changed > 0 || from_file) && waiting == 0 && !pool_blocked && st.project.as_ref().is_some_and(|p| p.read_only.is_none());
    let label = if wide { format!("⬆ Push to device ({changed})") } else { format!("⬆ {changed}") };
    let push = ui
        .add_enabled(can_push, egui::Button::new(RichText::new(label).color(if can_push { theme::DEVICE_ACCENT_BRIGHT } else { Color32::GRAY })))
        .on_hover_text(if waiting > 0 {
            format!("Waiting for {waiting} file(s) to reach or be found on the +Drive")
        } else if pool_blocked && !busy {
            "The read was stopped before every pattern and pool sound arrived, and sample pool changes rewrite all of them. Reload to push them, or reset the sample order to push just the pattern changes.".to_owned()
        } else if from_file {
            "Write the whole project from the file into the device's memory, replacing the project loaded there (the saved copy on +Drive is not touched)".to_owned()
        } else {
            format!(
                "Write {} sample pool, {} pattern and {} sound pool change(s) into the project loaded on the device (its memory, not the saved copy on +Drive)",
                st.pool_changed(),
                st.proj.changed(),
                st.lib.sound_changed()
            )
        });
    if push.clicked() {
        st.confirm_push = true;
    }
    projects_menu(ui, st, out, wide);
    file_menu(ui, st, out, wide);
    let close = theme::close_button(None, false);
    if ui.add_enabled(!busy, close).on_hover_text("Close this project here (the device keeps it)").clicked() {
        if st.changed() > 0 {
            st.confirm_close = true;
        } else {
            st.close_project();
        }
    }
}

/// Save / load project files, and the research dump.
fn file_menu(ui: &mut egui::Ui, st: &mut ElekState, out: &mut Vec<ElekIntent>, wide: bool) {
    let busy = st.busy.is_some();
    let can_save = !busy && st.project.as_ref().is_some_and(|p| !p.partial);
    ui.menu_button(if wide { "💾 File" } else { "💾" }, |ui| {
        if ui
            .add_enabled(can_save, egui::Button::new("Save project…"))
            .on_hover_text("The project as you see it here, unpushed changes included, as a JSON file. Samples are noted by their +Drive hash, not included.")
            .clicked()
        {
            out.push(ElekIntent::SaveProject(false));
            ui.close();
        }
        if ui
            .add_enabled(can_save, egui::Button::new("Save project with samples…"))
            .on_hover_text("A folder: the project file plus a samples/ folder with every pool sample's WAV, from local copies or fetched from the device. Loading it elsewhere uploads only the samples that device lacks (to /imported/<name>).")
            .clicked()
        {
            out.push(ElekIntent::SaveProject(true));
            ui.close();
        }
        if ui
            .add_enabled(!busy, egui::Button::new("Load project…"))
            .on_hover_text("Open a saved project file (or the .json inside a bundle folder) here. Its samples are checked on the device; bundled ones it lacks are uploaded. Nothing else goes to the device until you push.")
            .clicked()
        {
            out.push(ElekIntent::LoadProject);
            ui.close();
        }
        ui.separator();
        let can_dump = can_save && st.project.as_ref().is_some_and(|p| p.from_file.is_none());
        if ui
            .add_enabled(can_dump, egui::Button::new("Save debug dump"))
            .on_hover_text("The project as read (raw .syx, decoded structs, names and strings) to ~/Documents/sample_manager/project_dumps, for format research. Includes project pool and track sounds; saved +Drive presets have their own debug dump on Presets.")
            .clicked()
        {
            out.push(ElekIntent::SaveDump);
            ui.close();
        }
    });
}

/// The projects saved on the +Drive, as a dropdown. They can't be opened
/// from here: they're listed so you know what to load on the device.
fn projects_menu(ui: &mut egui::Ui, st: &mut ElekState, out: &mut Vec<ElekIntent>, wide: bool) {
    let n = match &st.projects {
        Some(Ok(v)) => format!(" ({})", v.len()),
        _ => String::new(),
    };
    let text = if wide { format!("+Drive projects{n}") } else { format!("+Drive{n}") };
    egui::ComboBox::from_id_salt("elek_drive_projects")
        .selected_text(text)
        .height(420.0)
        .width(if wide { 180.0 } else { 90.0 })
        .show_ui(ui, |ui| {
            ui.set_min_width(260.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Saved on the +Drive").strong());
                if ui.small_button("⟳").on_hover_text("List again").clicked() {
                    out.push(ElekIntent::ListProjects);
                }
            });
            ui.label(RichText::new("To work on one: load it on the device (PROJECT › LOAD), then Load / Reload here.").weak().small());
            ui.separator();
            match &st.projects {
                None => {
                    ui.spinner();
                }
                Some(Err(e)) => {
                    ui.colored_label(Color32::from_rgb(230, 120, 90), e);
                }
                Some(Ok(v)) if v.is_empty() => {
                    ui.label(RichText::new("No saved projects").font(oled_font(12.0)).color(OLED_DIM));
                }
                Some(Ok(v)) => {
                    for p in v {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("{:03}", p.slot)).font(oled_font(13.0)).color(OLED_DIM));
                            ui.label(RichText::new(&p.name).font(oled_font(13.0)).color(OLED));
                            ui.label(RichText::new(format!("{} KB", p.size / 1024)).font(oled_font(10.0)).color(OLED_DIM));
                        });
                    }
                }
            }
        });
}

/// The progress bar while the device works, else the last status.
pub(crate) fn status_line(ui: &mut egui::Ui, st: &ElekState, wide: bool) {
    if let Some((label, done, total)) = &st.busy {
        // Leave room for Stop: a bar that fills the row pushes the button
        // past the edge, and a side panel then grows to fit it.
        let stop_w = if st.reading.is_some() { 80.0 } else { 0.0 };
        let room = ui.available_width() - stop_w;
        let w = if wide { room.clamp(160.0, 420.0) } else { room.max(60.0) };
        ui.add(theme::progress_bar(*done as f32 / (*total).max(1) as f32).desired_width(w).text(format!("{label} {done}/{total}")));
        stop_read(ui, st);
    } else if !st.status.is_empty() && st.project.is_some() {
        ui.add(egui::Label::new(RichText::new(&st.status).weak()).truncate()).on_hover_text(&st.status);
    }
}

/// Undo / reset for the sample order.
pub(super) fn order_tools(ui: &mut egui::Ui, st: &mut ElekState) {
    let busy = st.edit_locked();
    let changed = st.pool_changed();
    let wide = !st.docked;
    if ui.add_enabled(!busy && !st.undo.is_empty(), egui::Button::new(if wide { "↶ Undo" } else { "↶" })).on_hover_text("Undo the last move (⌘Z)").clicked() {
        st.undo();
    }
    if ui.add_enabled(!busy && changed > 0, egui::Button::new(if wide { "↺ Reset" } else { "↺" })).on_hover_text("Back to the order as read from the device").clicked() {
        st.undo.push((st.order.clone(), st.fills.clone()));
        st.order = (0..st.order.len() as u16).collect();
        st.fills.clear();
        st.edited_at = Some(std::time::Instant::now());
    }
    // The whole project, read from the device: every pattern can follow
    // its samples, so a live push never leaves one on the wrong sample.
    let can_live = st.project.as_ref().is_some_and(|p| p.read_only.is_none() && p.from_file.is_none() && !p.partial);
    let was = st.live;
    ui.add_enabled_ui(can_live || st.live, |ui| {
        ui.toggle_value(&mut st.live, if wide { "⚡ Live" } else { "⚡" })
            .on_hover_text(
                "Push every sample order change straight away, without asking. Every pattern that uses a moved \
                 sample is renumbered to follow it, as with a normal push, so nothing plays the wrong sample. \
                 Changes made on the device since the last read are overwritten. Each push is read back in full \
                 before the next one goes.",
            )
            .on_disabled_hover_text("Needs the whole project read from the device (let the read finish, or reload)");
    });
    if st.live && !was {
        st.status = "Live: sample order changes push right away; every pattern follows its samples".into();
    }
}

/// Stop a read under way; the patterns that arrived stay.
fn stop_read(ui: &mut egui::Ui, st: &ElekState) {
    let Some(ctl) = &st.reading else { return };
    let stopping = ctl.stop.load(std::sync::atomic::Ordering::Relaxed);
    if ui
        .add_enabled(!stopping, egui::Button::new(if stopping { "Stopping…" } else { "Stop" }).small())
        .on_hover_text("Stop reading after the pattern on its way. Banks that loaded completely can be edited and their pattern changes pushed; sample order changes need a full read.")
        .clicked()
    {
        ctl.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// No project loaded: one big load button.
pub(crate) fn load_screen(ui: &mut egui::Ui, st: &mut ElekState, out: &mut Vec<ElekIntent>) {
    ui.add_space(40.0);
    ui.vertical_centered(|ui| {
        if let Some((label, done, total)) = &st.busy {
            ui.label(RichText::new(format!("{label}…")).font(oled_font(14.0)).color(OLED));
            ui.add(theme::progress_bar(*done as f32 / (*total).max(1) as f32).desired_width(260.0).text(format!("{done}/{total}")));
            stop_read(ui, st);
        } else {
            let big = egui::Button::new(RichText::new("⬇  Load current project from device memory").size(15.0).color(theme::DEVICE_ACCENT_BRIGHT))
                .min_size(egui::vec2(320.0, 36.0));
            if ui.add(big).clicked() {
                out.push(ElekIntent::Read);
            }
            ui.add_space(6.0);
            ui.label(RichText::new("Reads whatever project is open on the device now.\nTo work on another one, load it on the device first (PROJECT › LOAD).").weak());
            ui.add_space(10.0);
            if ui.button("📂  Open a project file…").on_hover_text("A project saved from here earlier. Nothing goes to the device until you push.").clicked() {
                out.push(ElekIntent::LoadProject);
            }
            if !st.status.is_empty() {
                ui.add_space(6.0);
                ui.colored_label(Color32::from_rgb(230, 120, 90), &st.status);
            }
        }
    });
}
