//! Drag and drop with Finder / Explorer.
//!
//! In: the window only learns *which* files are hovering or were dropped, not
//! where (macOS sends no mouse moves during an OS drag). So while files hover
//! we read the cursor's screen position ourselves, feed it to egui as pointer
//! moves, and put the files up as an ordinary `DragPayload`. On drop we add a
//! button release, and the existing drop targets (panes, folder rows, tab bar)
//! take it from there.
//!
//! Out (macOS only): when an in-app drag of computer files leaves the window,
//! it's handed to the OS as a real file drag, so it can land in Finder.

use super::App;
use crate::ui::file_table::DragPayload;
use egui::{DragAndDrop, Event, PointerButton, Pos2, RawInput};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Default)]
pub(super) struct OsDnd {
    /// Files from outside are hovering and we put a payload up for them.
    hovering: bool,
    /// Set when a drag we handed to the OS ends. The OS swallowed the mouse-up,
    /// so egui still thinks the button is held; we send it a release.
    ended: Arc<AtomicBool>,
}

/// Where the mouse is, in egui points relative to the window's content.
fn pointer_in_window(ctx: &egui::Context, raw: &RawInput) -> Option<Pos2> {
    use mouse_position::mouse_position::Mouse;
    let info = raw.viewports.get(&raw.viewport_id)?;
    let inner = info.inner_rect?;
    let Mouse::Position { x, y } = Mouse::get_mouse_position() else {
        return None;
    };
    // macOS reports logical points, the others physical pixels.
    let to_px = if cfg!(target_os = "macos") {
        info.native_pixels_per_point.unwrap_or(1.0)
    } else {
        1.0
    };
    let ppp = ctx.pixels_per_point();
    Some(Pos2::new(x as f32 * to_px / ppp, y as f32 * to_px / ppp) - inner.min.to_vec2())
}

impl App {
    /// Called before egui sees each frame's input.
    pub(super) fn os_dnd_input(&mut self, ctx: &egui::Context, raw: &mut RawInput) {
        if self.os_dnd.ended.swap(false, Ordering::Relaxed) {
            let pos = pointer_in_window(ctx, raw).unwrap_or(Pos2::new(-1.0, -1.0));
            raw.events.push(Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: ctx.input(|i| i.modifiers),
            });
        }

        let dropped: Vec<PathBuf> = raw
            .dropped_files
            .iter()
            .map(|f| f.path().to_path_buf())
            .collect();
        let hovered: Vec<PathBuf> = raw
            .hovered_files
            .iter()
            .filter_map(|f| f.path.clone())
            .collect();

        if !dropped.is_empty() {
            self.os_dnd.hovering = false;
            if let Some(pos) = pointer_in_window(ctx, raw) {
                DragAndDrop::set_payload(
                    ctx,
                    DragPayload {
                        paths: dropped,
                        device: false,
                        external: true,
                    },
                );
                raw.events.push(Event::PointerMoved(pos));
                raw.events.push(Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: false,
                    modifiers: ctx.input(|i| i.modifiers),
                });
            }
        } else if !hovered.is_empty() {
            if !self.os_dnd.hovering {
                self.os_dnd.hovering = true;
                DragAndDrop::set_payload(
                    ctx,
                    DragPayload {
                        paths: hovered,
                        device: false,
                        external: true,
                    },
                );
            }
            if let Some(pos) = pointer_in_window(ctx, raw) {
                raw.events.push(Event::PointerMoved(pos));
            }
            // No window events arrive while the OS drags, so keep frames coming
            // to follow the cursor.
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        } else if self.os_dnd.hovering {
            // Dragged back out of the window, or cancelled.
            self.os_dnd.hovering = false;
            DragAndDrop::clear_payload(ctx);
        }
    }

    /// Hand an in-app drag of computer files to the OS once it leaves the window.
    #[cfg(target_os = "macos")]
    pub(super) fn os_drag_out(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        let Some(p) = DragAndDrop::payload::<DragPayload>(ctx) else {
            return;
        };
        if p.device || p.external {
            return;
        }
        let Some(pos) = ctx.input(|i| i.pointer.latest_pos()) else {
            return;
        };
        if ctx.content_rect().contains(pos) {
            return;
        }
        DragAndDrop::clear_payload(ctx);
        let ended = self.os_dnd.ended.clone();
        let repaint = ctx.clone();
        let res = drag::start_drag(
            frame,
            drag::DragItem::Files(p.paths.clone()),
            drag::Image::Raw(include_bytes!("../../assets/drag.png").to_vec()),
            move |_result, _pos| {
                ended.store(true, Ordering::Relaxed);
                repaint.request_repaint();
            },
            drag::Options::default(),
        );
        if let Err(e) = res {
            self.errors
                .push(format!("couldn't start drag to Finder: {e}"));
        }
    }
}
