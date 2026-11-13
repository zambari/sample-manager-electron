//! The strips above and below the panes: device picker, volume and meter,
//! toolbar, status bar.

use super::*;

impl App {
    pub(super) fn device_picker(&mut self, ui: &mut egui::Ui) {
        let mut choice: Option<Option<String>> = None;
        let name = if self.engine.device_name.is_empty() {
            "no output".to_owned()
        } else {
            self.engine.device_name.clone()
        };
        if ui
            .small_button("⟳")
            .on_hover_text("Rescan audio devices")
            .clicked()
        {
            self.devices = Engine::devices();
        }
        egui::ComboBox::from_id_salt("output_device")
            .selected_text(format!("🔈 {name}"))
            .width(220.0)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(self.output_device.is_none(), "System default")
                    .clicked()
                {
                    choice = Some(None);
                }
                for d in &self.devices {
                    let on = self.output_device.as_deref() == Some(d.id.as_str());
                    if ui
                        .selectable_label(on, &d.name)
                        .on_hover_text(&d.id)
                        .clicked()
                    {
                        choice = Some(Some(d.id.clone()));
                    }
                }
            });
        ui.separator();
        self.volume_and_meter(ui);
        if let Some(dev) = choice {
            self.engine.switch(dev.as_deref());
            self.output_device = dev;
            self.last_stream_error = None;
            if let Some(e) = &self.engine.open_error {
                self.errors.push(format!("audio output: {e}"));
            }
        }
    }

    /// Master volume slider, then a black strip running to the right-hand
    /// controls. The L/R level meter sits on the strip; while a stereo sample
    /// plays, the strip shows a wide, flat slice of a phase scope.
    fn volume_and_meter(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().slider_width = 80.0;
        const VOL_MAX: f32 = 1.25;
        let slider = egui::Slider::new(&mut self.volume, 0.0..=VOL_MAX)
            .show_value(false)
            .trailing_fill(true);
        let r = ui.add(slider);
        // Tick marks at unity gain, above and below the rail. Mirrors egui's
        // handle inset (radius = height / 2.5) so the tick lines up.
        {
            let rect = r.rect;
            let inset = rect.height() / 2.5;
            let x = rect.left() + inset + (rect.width() - 2.0 * inset) / VOL_MAX;
            let stroke = egui::Stroke::new(1.0, ui.visuals().weak_text_color());
            let p = ui.painter();
            p.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.top() + 3.0)], stroke);
            p.line_segment([egui::pos2(x, rect.bottom() - 3.0), egui::pos2(x, rect.bottom())], stroke);
        }
        let r = r.on_hover_text(format!(
            "Output volume {:.2} (not saved). Tick marks 1.0; double-click to reset to it",
            self.volume
        ));
        if r.double_clicked() {
            self.volume = 1.0;
        }
        self.engine.set_volume(self.volume);
        self.pitch_fader(ui);

        // Filled in once the meter is placed and the strip's width is known,
        // but painted beneath the meter.
        let strip_left = ui.cursor().left();
        let strip_bg = ui.painter().add(egui::Shape::Noop);

        // Fast attack, ~24 dB/s fall; the hold mark falls slower.
        let dt = ui.input(|i| i.stable_dt).min(0.1);
        let peaks = self.engine.take_peaks();
        let fall = 10f32.powf(-24.0 * dt / 20.0);
        let hold_fall = 10f32.powf(-8.0 * dt / 20.0);
        for k in 0..2 {
            self.meter[k] = peaks[k].max(self.meter[k] * fall);
            self.meter_hold[k] = peaks[k].max(self.meter_hold[k] * hold_fall);
        }

        let (rect, resp) = ui.allocate_exact_size(egui::vec2(96.0, 12.0), egui::Sense::hover());
        let p = ui.painter();
        p.rect_filled(rect, 1.0, ui.visuals().extreme_bg_color);
        const FLOOR_DB: f32 = -48.0;
        let x_of = |lin: f32| {
            let db = crate::audio::stats::to_db(lin.max(1e-6));
            rect.left() + rect.width() * ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0)
        };
        // Colour zones: green up to -9 dB, orange to -1 dB, red at the top.
        let zones = [
            (FLOOR_DB, -9.0, egui::Color32::from_rgb(96, 168, 104)),
            (-9.0, -1.0, theme::ACCENT_BRIGHT),
            (-1.0, 0.0, egui::Color32::from_rgb(222, 60, 50)),
        ];
        let bar_h = (rect.height() - 1.0) / 2.0;
        for k in 0..2 {
            let y0 = rect.top() + k as f32 * (bar_h + 1.0);
            let lvl = x_of(self.meter[k]);
            for (a, b, c) in zones {
                let xa = x_of(crate::audio::stats::from_db(a));
                let xb = x_of(crate::audio::stats::from_db(b)).min(lvl);
                if xb > xa {
                    p.rect_filled(
                        egui::Rect::from_x_y_ranges(xa..=xb, y0..=y0 + bar_h),
                        0.0,
                        c,
                    );
                }
            }
            if self.meter_hold[k] > 1e-4 {
                let x = x_of(self.meter_hold[k]);
                p.line_segment(
                    [egui::pos2(x, y0), egui::pos2(x, y0 + bar_h)],
                    egui::Stroke::new(1.0, egui::Color32::from_gray(220)),
                );
            }
        }
        // Tick marks at -24, -12, -6 and 0 dB.
        for db in [-24.0, -12.0, -6.0] {
            let x = x_of(crate::audio::stats::from_db(db));
            p.line_segment(
                [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                egui::Stroke::new(1.0, egui::Color32::from_black_alpha(140)),
            );
        }
        let hold_db =
            crate::audio::stats::to_db(self.meter_hold[0].max(self.meter_hold[1]).max(1e-6));
        resp.on_hover_text(format!(
            "Output level, top L, bottom R (-48..0 dBFS)\nPeak {}",
            if hold_db < -90.0 {
                "-inf dB".to_owned()
            } else {
                format!("{hold_db:.1} dB")
            }
        ));

        let active = self.engine.is_playing() || self.meter_hold.iter().any(|&h| h > 1e-4);
        if active {
            ui.ctx().request_repaint();
        }

        // From the slider to the right-hand controls, full bar height.
        // Bar's clip rect gives its edges; capped near the row in case the
        // clip is wider than the panel.
        let ys = ui
            .clip_rect()
            .intersect(ui.max_rect().expand2(egui::vec2(0.0, 8.0)))
            .y_range();
        let strip = egui::Rect::from_x_y_ranges(strip_left..=ui.max_rect().right(), ys);
        if strip.width() > 4.0 {
            let mut shapes = vec![egui::Shape::rect_filled(strip, 0.0, egui::Color32::BLACK)];
            if self.engine.is_stereo() {
                self.phase_strip(strip, &mut shapes);
            }
            ui.painter().set(strip_bg, egui::Shape::Vec(shapes));
        }
    }

    /// DJ-style pitch fader: right is faster. Speeds pitch and tempo
    /// together over ±50 %. Double-click (or the readout) recentres it.
    fn pitch_fader(&mut self, ui: &mut egui::Ui) {
        let range = PITCH_RANGE;
        let (rect, resp) =
            ui.allocate_exact_size(egui::vec2(110.0, 16.0), egui::Sense::click_and_drag());
        let x_of = |p: f32| rect.left() + rect.width() * (p / range * 0.5 + 0.5).clamp(0.0, 1.0);
        if resp.dragged() || (resp.clicked() && !resp.double_clicked()) {
            if let Some(pos) = resp.interact_pointer_pos() {
                let fine = ui.input(|i| i.modifiers.shift);
                let p = if fine && resp.dragged() {
                    // Shift-drag: tenth of the speed for fine nudges.
                    self.pitch + resp.drag_delta().x / rect.width() * 2.0 * range * 0.1
                } else {
                    ((pos.x - rect.left()) / rect.width() * 2.0 - 1.0) * range
                };
                // A small dead zone snaps to centre, like a fader detent.
                self.pitch = if p.abs() < range * 0.015 { 0.0 } else { p.clamp(-range, range) };
            }
        }
        if resp.double_clicked() {
            self.pitch = 0.0;
        }
        if resp.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y + i.smooth_scroll_delta.x);
            if scroll != 0.0 {
                // 0.1% per notch-ish.
                self.pitch = (self.pitch + scroll.signum() * 0.001).clamp(-range, range);
                if self.pitch.abs() < 0.0005 {
                    self.pitch = 0.0;
                }
            }
        }
        self.pitch = self.pitch.clamp(-range, range);

        let p = ui.painter();
        let v = ui.visuals();
        let cy = rect.center().y;
        // Track slot.
        p.rect_filled(
            egui::Rect::from_x_y_ranges(rect.left()..=rect.right(), cy - 1.5..=cy + 1.5),
            1.0,
            v.extreme_bg_color,
        );
        // Ticks: centre detent tall, quarters short.
        for k in -4..=4 {
            let x = x_of(range * k as f32 / 4.0);
            let h = if k == 0 { 7.0 } else if k % 2 == 0 { 4.0 } else { 2.5 };
            p.line_segment(
                [egui::pos2(x, cy - h), egui::pos2(x, cy + h)],
                egui::Stroke::new(1.0, v.weak_text_color().gamma_multiply(if k == 0 { 1.0 } else { 0.6 })),
            );
        }
        // Fill from centre to the cap when off-centre.
        let cx = x_of(self.pitch);
        if self.pitch != 0.0 {
            let mid = x_of(0.0);
            p.rect_filled(
                egui::Rect::from_x_y_ranges(mid.min(cx)..=mid.max(cx), cy - 1.5..=cy + 1.5),
                0.0,
                theme::ACCENT,
            );
        }
        // Fader cap with a centre line.
        let cap = egui::Rect::from_center_size(egui::pos2(cx, cy), egui::vec2(8.0, rect.height()));
        let cap_fill = if resp.hovered() || resp.dragged() {
            egui::Color32::from_gray(200)
        } else {
            egui::Color32::from_gray(160)
        };
        p.rect_filled(cap, 1.5, cap_fill);
        p.line_segment(
            [egui::pos2(cx, cap.top() + 2.0), egui::pos2(cx, cap.bottom() - 2.0)],
            egui::Stroke::new(
                1.0,
                if self.pitch == 0.0 { egui::Color32::from_rgb(96, 168, 104) } else { theme::ACCENT },
            ),
        );
        resp.on_hover_text(
            "Pitch (speed). Drag or scroll; Shift-drag for fine.\nDouble-click to reset. Resets on each new sample.",
        );

        // Readout: click to reset.
        let txt = if self.pitch == 0.0 {
            " 0.0%".to_owned()
        } else {
            format!("{:+.1}%", self.pitch * 100.0)
        };
        let mut rt = egui::RichText::new(txt).monospace().small();
        if self.pitch != 0.0 {
            rt = rt.color(theme::ACCENT_BRIGHT);
        }
        if ui
            .add(egui::Label::new(rt).sense(egui::Sense::click()))
            .on_hover_text("Click to reset pitch")
            .clicked()
        {
            self.pitch = 0.0;
        }
        self.engine.set_speed(1.0 + self.pitch);
    }

    /// Goniometer (mid up, side across) scaled so side spans the strip's
    /// width; only a thin slice around the centre fits the height. Mono shows
    /// as a vertical line, wide stereo spreads out sideways. A thin L/R
    /// correlation bar runs along the bottom edge.
    fn phase_strip(&self, strip: egui::Rect, shapes: &mut Vec<egui::Shape>) {
        let frames = self.engine.scope(1024);
        let c = strip.center();
        let r = strip.width() / 2.0;
        let s = std::f32::consts::FRAC_1_SQRT_2;
        let guide = egui::Stroke::new(1.0, egui::Color32::from_gray(28));
        shapes.push(egui::Shape::line_segment(
            [
                egui::pos2(c.x, strip.top()),
                egui::pos2(c.x, strip.bottom()),
            ],
            guide,
        ));

        // Cut the trace into runs that stay inside the strip, so lines don't
        // get drawn across the clipped-away parts.
        let stroke = egui::Stroke::new(1.0, theme::WAVE.gamma_multiply(0.45));
        let mut run: Vec<egui::Pos2> = Vec::new();
        for &(l, rr) in &frames {
            let pt = c + egui::vec2((rr - l) * s, -(l + rr) * s) * r;
            if strip.contains(pt) {
                run.push(pt);
            } else if !run.is_empty() {
                if run.len() > 1 {
                    shapes.push(egui::Shape::line(std::mem::take(&mut run), stroke));
                }
                run.clear();
            }
        }
        if run.len() > 1 {
            shapes.push(egui::Shape::line(run, stroke));
        }

        // Correlation: +1 mono, 0 wide, -1 out of phase.
        let (mut lr, mut ll, mut rr2) = (0.0f32, 0.0f32, 0.0f32);
        for &(l, rr) in &frames {
            lr += l * rr;
            ll += l * l;
            rr2 += rr * rr;
        }
        if ll * rr2 > 1e-12 {
            let corr = (lr / (ll * rr2).sqrt()).clamp(-1.0, 1.0);
            let x = c.x + corr * strip.width() / 2.0;
            let col = if corr < 0.0 {
                egui::Color32::from_rgb(222, 60, 50)
            } else {
                theme::ACCENT_BRIGHT
            };
            let bar = egui::Rect::from_x_y_ranges(
                c.x.min(x)..=c.x.max(x),
                strip.bottom() - 2.0..=strip.bottom(),
            );
            shapes.push(egui::Shape::rect_filled(bar, 0.0, col.gamma_multiply(0.6)));
        }
    }

    pub(super) fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("SAMPLE MANAGER").size(13.0).strong().color(theme::ACCENT_BRIGHT));

            if self.build_watch.ready() {
                ui.separator();
                let idle = self.active_jobs.is_empty();
                let btn = egui::Button::new(
                    egui::RichText::new("New build: relaunch").color(theme::ON_ACCENT),
                )
                .fill(theme::ACCENT);
                let resp = ui
                    .add_enabled(idle, btn)
                    .on_hover_text("A newer build is on disk. Quit (asking about unsaved edits first) and start it.")
                    .on_disabled_hover_text("Wait for the transfer to finish");
                if resp.clicked() {
                    self.relaunch = true;
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Right to left: the first widget lands at the far right.
                let commit_key = self.keymap.describe(Action::Commit);
                let commit_btn = egui::Button::new(
                    egui::RichText::new(format!("Commit to destination  {commit_key}")).color(theme::ON_ACCENT),
                )
                .fill(theme::ACCENT);
                if ui
                    .add(commit_btn)
                    .on_hover_text(
                        "Copy the source selection into the active destination tab.\n\
                         A single file open in the editor is written with its trim, gain and mono settings.",
                    )
                    .clicked()
                {
                    self.commit(Kind::Copy);
                }
                if ui.button("Move").clicked() {
                    self.commit(Kind::Move);
                }
                ui.separator();
                ui.label(
                    egui::RichText::new(format!("keys: {}", self.keymap.name))
                        .weak()
                        .small(),
                );
                ui.separator();
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    self.device_picker(ui);
                });
            });
        });
    }

    /// The job the status bar shows: the oldest one that is running.
    pub(super) fn shown_job(&self) -> Option<&ActiveJob> {
        self.active_jobs
            .iter()
            .find(|a| a.started)
            .or(self.active_jobs.first())
    }

    pub(super) fn status_bar(&mut self, ui: &mut egui::Ui) {
        // Anything shown as an error also goes to the log, once.
        for e in self.errors.iter().skip(self.errors_logged) {
            crate::log::line(format!("error: {e}"));
        }
        self.errors_logged = self.errors.len();

        let waiting = self.active_jobs.len().saturating_sub(1);
        // The ETA clock starts when the first job of the run starts moving.
        if self.batch.started_at.is_none() && self.active_jobs.iter().any(|j| j.started) {
            self.batch.started_at = Some(std::time::Instant::now());
        }
        if let Some(a) = self.shown_job() {
            // The whole queue as one run: finished jobs plus every live one.
            // A waiting device job counts its top-level items until it starts
            // and counts the files inside, so the total can grow.
            let b = &self.batch;
            let done_items =
                b.done_items + self.active_jobs.iter().map(|j| j.done_items).sum::<usize>();
            let total_items = b.total_items
                + self
                    .active_jobs
                    .iter()
                    .map(|j| j.total_items)
                    .sum::<usize>();
            let done_bytes =
                b.done_bytes + self.active_jobs.iter().map(|j| j.done_bytes).sum::<u64>();
            let total_bytes =
                b.total_bytes + self.active_jobs.iter().map(|j| j.total_bytes).sum::<u64>();
            let bytes_known =
                !b.bytes_unknown && self.active_jobs.iter().all(|j| j.total_bytes > 0);
            let frac = if bytes_known && total_bytes > 0 {
                done_bytes as f32 / total_bytes as f32
            } else if total_items > 0 {
                done_items as f32 / total_items as f32
            } else {
                0.0
            };
            let job_id = a.id;
            let file = a.file;
            // Once a device job has shown a per-file bar, keep it for the
            // rest of the queue so the main bar doesn't resize between files.
            let file_bar = self.active_jobs.iter().any(|j| j.file_bar) || self.batch.file_bar;
            // Device folders have no known size up front: show bytes so far.
            let bytes = if bytes_known {
                format!(
                    "{} / {}",
                    fs_model::human_bytes(done_bytes),
                    fs_model::human_bytes(total_bytes)
                )
            } else {
                fs_model::human_bytes(done_bytes)
            };
            let label = if a.started || !a.label.is_empty() {
                a.label.as_str()
            } else {
                "Starting…"
            };
            // Average rate over the run so far times the bytes still to go.
            // Wait a few seconds so the first file's ramp-up doesn't skew it.
            let eta = b
                .started_at
                .map(|t| t.elapsed().as_secs_f64())
                .filter(|&secs| bytes_known && secs >= 3.0 && done_bytes > 0)
                .map(|secs| {
                    let left = total_bytes.saturating_sub(done_bytes) as f64;
                    let mins = (left * secs / done_bytes as f64 / 60.0).ceil() as u64;
                    if mins <= 1 {
                        " · <1 min left".to_owned()
                    } else {
                        format!(" · ~{mins} min left")
                    }
                })
                .unwrap_or_default();
            let text = format!(
                "{}/{} files · {bytes}{eta} · {label}",
                done_items.min(total_items),
                total_items
            );
            // Buttons pinned right; the bar takes the rest so long names fit.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if waiting > 0 && ui.button("Cancel all").clicked() {
                    for j in &self.active_jobs {
                        self.jobs.cancel(j.id);
                    }
                }
                if ui
                    .button("Cancel")
                    .on_hover_text("Stop the job running now")
                    .clicked()
                {
                    self.jobs.cancel(job_id);
                }
                if waiting > 0 {
                    ui.label(format!("+{waiting} queued")).on_hover_text(
                        "Waiting jobs start when this one finishes. Their files count in the bar.",
                    );
                }
                // This file, chunk by chunk; an empty bar between files.
                if file_bar {
                    let (frac, text) = match file.filter(|f| f.2 > 0) {
                        Some((phase, done, total)) => (
                            done as f32 / total as f32,
                            format!(
                                "{phase} {} / {}",
                                fs_model::human_bytes(done.min(total)),
                                fs_model::human_bytes(total)
                            ),
                        ),
                        None => (0.0, "preparing…".to_owned()),
                    };
                    ui.add(
                        theme::progress_bar(frac.clamp(0.0, 1.0))
                            .desired_width(200.0)
                            .text(text),
                    )
                    .on_hover_text("Current file");
                }
                ui.add(
                    theme::progress_bar(frac.clamp(0.0, 1.0))
                        .desired_width(ui.available_width())
                        .text(text),
                );
            });
            return;
        }

        ui.horizontal(|ui| {
            // Counts live in each pane's own footer; here just say which
            // side has focus.
            ui.label(if self.editor_focused {
                "Editor"
            } else {
                self.focused.label()
            });

            if let Some(p) = self.preview.playing() {
                ui.separator();
                ui.label(format!(
                    "▶ {}",
                    p.file_name().unwrap_or_default().to_string_lossy()
                ));
            }

            if !self.status.is_empty() {
                ui.separator();
                ui.label(egui::RichText::new(&self.status).weak());
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(last) = self.errors.last() {
                    if ui
                        .button(
                            egui::RichText::new(format!("⚠ {}", truncate(last, 60)))
                                .color(ui.visuals().error_fg_color),
                        )
                        .on_hover_text(format!(
                            "{}\n\nclick to copy all to the clipboard and clear",
                            self.errors.join("\n")
                        ))
                        .clicked()
                    {
                        let n = self.errors.len();
                        ui.ctx().copy_text(self.errors.join("\n"));
                        self.status = format!(
                            "Copied {n} warning{} to the clipboard",
                            if n == 1 { "" } else { "s" }
                        );
                        self.errors.clear();
                        self.errors_logged = 0;
                    }
                }
            });
        });
    }
}

/// Pitch fader range, as a fraction each side (±50 %).
const PITCH_RANGE: f32 = 0.5;
