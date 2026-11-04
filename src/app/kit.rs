//! The slice kit: a second editor slot that collects slices picked from the
//! sample being browsed. `editor` is always the slot on screen and `other`
//! the hidden one; [`App::show_kit`] swaps them. The clipboard and prefs
//! stay with the screen, so copy / paste work across both.

use super::*;
use crate::editor::AudioClip;

/// A slice waiting on the "name the kit" question.
pub(super) struct KitNaming {
    pub clip: AudioClip,
    pub bits: Option<u32>,
    pub name: String,
    pub dir: PathBuf,
}

impl App {
    pub(super) fn kit(&self) -> &Editor {
        if self.showing_kit { &self.editor } else { &self.other }
    }

    fn kit_mut(&mut self) -> &mut Editor {
        if self.showing_kit { &mut self.editor } else { &mut self.other }
    }

    /// The slot that follows the file cursor.
    pub(super) fn browser(&self) -> &Editor {
        if self.showing_kit { &self.other } else { &self.editor }
    }

    pub(super) fn has_kit(&self) -> bool {
        let k = self.kit();
        k.current.is_some() || k.loading.is_some()
    }

    pub(super) fn kit_name(&self) -> Option<String> {
        let k = self.kit();
        let p = k.path().or(k.loading.as_deref())?;
        Some(p.file_name().unwrap_or_default().to_string_lossy().into_owned())
    }

    /// Put the kit (`true`) or the browsed sample on screen.
    pub(super) fn show_kit(&mut self, on: bool) {
        if on == self.showing_kit {
            return;
        }
        if on && !self.has_kit() {
            self.status = "No slice kit yet — K adds the selection to one".to_owned();
            return;
        }
        self.swap_slots();
        if self.engine.is_playing() {
            self.engine.stop();
        }
    }

    pub(super) fn swap_slots(&mut self) {
        std::mem::swap(&mut self.editor, &mut self.other);
        std::mem::swap(&mut self.editor.clip, &mut self.other.clip);
        std::mem::swap(&mut self.editor.prefs, &mut self.other.prefs);
        self.showing_kit = !self.showing_kit;
    }

    /// K: add the browsed sample's selection to the end of the kit, or ask
    /// for a kit name when there's none yet.
    pub(super) fn add_slice(&mut self) {
        if self.showing_kit {
            self.status = "Showing the kit — press B to go back to the sample, then K adds its selection".to_owned();
            return;
        }
        let Some(clip) = self.editor.slice() else {
            self.status = "Load a sample first, then K adds its selection to the slice kit".to_owned();
            return;
        };
        if self.other.loading.is_some() {
            self.status = "The slice kit is still loading — try again in a moment".to_owned();
            return;
        }
        if self.other.current.is_some() {
            let secs = clip.frames() as f64 / clip.rate.max(1) as f64;
            self.other.append(&clip);
            self.status = format!(
                "Added {secs:.3}s to {} ({:.2}s) — B shows the kit, ⌘S there saves it",
                self.kit_name().unwrap_or_default(),
                self.other.sample().map_or(0.0, |s| s.seconds(s.frames())),
            );
            return;
        }
        let bits = self.editor.sample().and_then(|s| s.bits);
        let src = self.editor.path().map(Path::to_path_buf).unwrap_or_default();
        let folder = src.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned());
        let name = folder.map_or_else(|| "slice kit".to_owned(), |f| format!("{f} kit"));
        self.kit_naming = Some(KitNaming { clip, bits, name, dir: self.kit_dir(&src) });
    }

    /// New kits go in the active destination tab when it's a local folder,
    /// else beside the sample they start from.
    fn kit_dir(&self, src: &Path) -> PathBuf {
        let tab = self.pane(Role::Dest).active_tab();
        if !tab.device && !tab.loading {
            return tab.cwd.clone();
        }
        src.parent().map_or_else(dirs_home, Path::to_path_buf)
    }

    /// The name was given: start the kit, or add to the file of that name.
    pub(super) fn start_kit(&mut self, n: KitNaming) {
        let mut name = n.name.trim().replace(['/', '\\', ':'], "-");
        if name.to_lowercase().ends_with(".wav") {
            name.truncate(name.len() - 4);
        }
        if name.is_empty() {
            name = "slice kit".to_owned();
        }
        let path = n.dir.join(format!("{name}.wav"));
        let file = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        self.kit_mut().close();
        if path.is_file() {
            self.kit_pending = Some(n.clip);
            self.kit_mut().request(&path);
            self.status = format!("{file} already exists — adding the slice to its end…");
        } else {
            self.kit_pending = None;
            self.kit_mut().start_new(path, &n.clip, n.bits);
            self.status = format!("Started {file} (not saved yet) — K adds more, B shows it, ⌘S there saves it");
        }
    }

    /// "Use as kit": the file on screen becomes the kit, to add slices to.
    pub(super) fn use_as_kit(&mut self) {
        if self.showing_kit {
            return;
        }
        let Some(p) = self.editor.path().map(Path::to_path_buf) else { return };
        if self.editor.is_dirty() {
            self.status = "Save this file first, then make it the kit".to_owned();
            return;
        }
        if self.other.is_dirty() {
            self.status = "The current kit has unsaved slices — save or close it first".to_owned();
            return;
        }
        self.kit_pending = None;
        self.other.close();
        self.other.request(&p);
        let name = p.file_name().unwrap_or_default().to_string_lossy();
        self.status = format!("{name} is the slice kit — K adds the selection of whatever sample you're on");
    }

    pub(super) fn close_kit(&mut self) {
        if self.kit().is_dirty() {
            self.status = "The kit has unsaved slices — save it (⌘S) first".to_owned();
            return;
        }
        self.show_kit(false);
        self.kit_pending = None;
        self.other.close();
        self.status = "Closed the slice kit".to_owned();
    }

    /// Absorb worker results for both slots. The hidden one is swapped in
    /// for the duration, since the handling is written against `editor`.
    pub(super) fn absorb_editors(&mut self) {
        self.absorb_editor(self.showing_kit);
        std::mem::swap(&mut self.editor, &mut self.other);
        self.absorb_editor(!self.showing_kit);
        std::mem::swap(&mut self.editor, &mut self.other);
    }
}
