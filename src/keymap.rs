//! All key handling in one place, with per-platform profiles.
//!
//! The point of this indirection: Finder and Explorer disagree about what
//! Enter, F2 and the primary modifier mean. Branching on `cfg!` at each call
//! site makes that impossible to keep straight -- and impossible to let the
//! user rebind later.

use egui::{InputState, Key, Modifiers};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    /// The core verb of this app: commit the source selection into the active
    /// destination tab.
    Commit,
    /// Begin inline rename (one item only).
    Rename,
    /// Open the Multi-Rename Tool (find / replace) over the selection.
    MultiRename,
    /// Descend into the directory under the cursor.
    OpenDir,
    GoUp,
    /// → : expand the folder under the cursor in tree mode, else `OpenDir`.
    Expand,
    /// ← : collapse / jump to the parent row in tree mode, else `GoUp`.
    Collapse,
    /// Start a recursive size walk for the selection.
    ComputeSize,
    Trash,
    /// Delete from the Digitakt right away, skipping /trash.
    DeleteNow,
    SelectAll,
    /// Take back the last file operation on the computer.
    Undo,
    /// Add/remove the cursor row from the selection without moving it.
    ToggleSelect,
    Refresh,
    CursorUp,
    CursorDown,
    ExtendUp,
    ExtendDown,
    NewTab,
    CloseTab,
    NextTab,
    PrevTab,
    SwitchPane,
    Cancel,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Chord {
    pub key: Key,
    pub mods: Modifiers,
}

const fn plain(key: Key) -> Chord {
    Chord { key, mods: Modifiers::NONE }
}
const fn cmd(key: Key) -> Chord {
    Chord { key, mods: Modifiers::COMMAND }
}
const fn shift(key: Key) -> Chord {
    Chord { key, mods: Modifiers::SHIFT }
}

pub struct Keymap {
    pub name: &'static str,
    pub bindings: Vec<(Chord, Action)>,
}

impl Keymap {
    /// Bindings shared by both platforms. `Modifiers::COMMAND` already maps to
    /// ⌘ on macOS and Ctrl elsewhere, so most entries need no duplication.
    fn common() -> Vec<(Chord, Action)> {
        vec![
            (plain(Key::Enter), Action::Commit),
            (plain(Key::ArrowUp), Action::CursorUp),
            (plain(Key::ArrowDown), Action::CursorDown),
            (shift(Key::ArrowUp), Action::ExtendUp),
            (shift(Key::ArrowDown), Action::ExtendDown),
            (plain(Key::ArrowRight), Action::Expand),
            (plain(Key::ArrowLeft), Action::Collapse),
            (plain(Key::Escape), Action::Cancel),
            (cmd(Key::A), Action::SelectAll),
            (cmd(Key::Z), Action::Undo),
            (cmd(Key::I), Action::ComputeSize),
            (cmd(Key::R), Action::Refresh),
            (cmd(Key::T), Action::NewTab),
            (cmd(Key::W), Action::CloseTab),
            (plain(Key::Tab), Action::SwitchPane),
            (cmd(Key::Insert), Action::ToggleSelect),
        ]
    }

    pub fn macos() -> Self {
        let mut bindings = Self::common();
        bindings.extend([
            // Finder: Enter renames -- but here Enter is Commit, so rename
            // keeps F2 and gains ⌘↩ as the Finder-ish alternative.
            (plain(Key::F2), Action::Rename),
            (shift(Key::F2), Action::MultiRename),
            (cmd(Key::Enter), Action::Rename),
            (cmd(Key::ArrowDown), Action::OpenDir),
            (cmd(Key::ArrowUp), Action::GoUp),
            (cmd(Key::Backspace), Action::Trash),
            (Chord { key: Key::Backspace, mods: Modifiers { alt: true, ..Modifiers::COMMAND } }, Action::DeleteNow),
            (cmd(Key::CloseBracket), Action::NextTab),
            (cmd(Key::OpenBracket), Action::PrevTab),
            // ⌃A as well as ⌘A, for hands used to other platforms.
            (Chord { key: Key::A, mods: Modifiers::CTRL }, Action::SelectAll),
        ]);
        Self { name: "macOS", bindings }
    }

    pub fn windows() -> Self {
        let mut bindings = Self::common();
        bindings.extend([
            (plain(Key::F2), Action::Rename),
            (shift(Key::F2), Action::MultiRename),
            (plain(Key::F5), Action::Refresh),
            (plain(Key::Backspace), Action::GoUp),
            (plain(Key::Delete), Action::Trash),
            (shift(Key::Delete), Action::DeleteNow),
            (Chord { key: Key::Tab, mods: Modifiers::CTRL }, Action::NextTab),
            (
                Chord {
                    key: Key::Tab,
                    mods: Modifiers { ctrl: true, shift: true, ..Modifiers::NONE },
                },
                Action::PrevTab,
            ),
        ]);
        Self { name: "Windows", bindings }
    }

    pub fn platform_default() -> Self {
        if cfg!(target_os = "macos") {
            Self::macos()
        } else {
            Self::windows()
        }
    }

    /// Resolve this frame's key presses to actions.
    ///
    /// Matches modifiers exactly, so ⌘↩ doesn't also fire plain ↩. More
    /// specific chords are checked first for the same reason.
    ///
    /// Not `==`: egui reports ⌘ as `mac_cmd + command` and Windows Ctrl as
    /// `ctrl + command`, so a plain comparison against `Modifiers::COMMAND`
    /// never matches. `matches_exact` knows about that.
    pub fn actions(&self, i: &InputState) -> Vec<Action> {
        let mut hits: Vec<(usize, Action)> = Vec::new();
        for (chord, action) in &self.bindings {
            if i.key_pressed(chord.key) && i.modifiers.matches_exact(chord.mods) {
                let specificity = usize::from(chord.mods.any());
                hits.push((specificity, *action));
            }
        }
        hits.sort_by(|a, b| b.0.cmp(&a.0));
        hits.into_iter().map(|(_, a)| a).collect()
    }

    pub fn describe(&self, action: Action) -> String {
        self.bindings
            .iter()
            .find(|(_, a)| *a == action)
            .map(|(c, _)| format_chord(*c))
            .unwrap_or_default()
    }
}

pub fn format_chord(c: Chord) -> String {
    let mut s = String::new();
    let mac = cfg!(target_os = "macos");
    if c.mods.command || c.mods.mac_cmd {
        s.push_str(if mac { "⌘" } else { "Ctrl+" });
    } else if c.mods.ctrl {
        s.push_str(if mac { "⌃" } else { "Ctrl+" });
    }
    if c.mods.shift {
        s.push_str(if mac { "⇧" } else { "Shift+" });
    }
    if c.mods.alt {
        s.push_str(if mac { "⌥" } else { "Alt+" });
    }
    s.push_str(match c.key {
        Key::Enter => "↩",
        Key::Space => "Space",
        Key::Backspace => if mac { "⌫" } else { "Backspace" },
        Key::ArrowUp => "↑",
        Key::ArrowDown => "↓",
        Key::ArrowLeft => "←",
        Key::ArrowRight => "→",
        other => other.name(),
    });
    s
}

// ---- sample editor ----------------------------------------------------------

/// How far a nudge moves the playhead.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Nudge {
    /// One screen pixel at the current zoom, so precision follows zoom.
    Pixel,
    TenPixels,
    /// Exactly one sample frame.
    Frame,
}

/// Keys while the editor column has focus. Kept apart from [`Action`] because
/// the same keys (Space, arrows, Enter) mean different things here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EdAction {
    TogglePlay,
    /// Play from the other start point than the toolbar setting says.
    TogglePlayAlt,
    ToggleSnap,
    ToggleFollow,
    /// Step the in / out point: to the next zero crossing with snap on,
    /// one pixel otherwise.
    InZero(i8),
    OutZero(i8),
    Crop,
    Uncrop,
    Undo,
    Redo,
    Stop,
    SetIn,
    SetOut,
    ToggleLoop,
    ToggleMono,
    Nudge(Nudge, i8),
    JumpIn,
    JumpOut,
    ZoomIn,
    ZoomOut,
    ZoomFit,
    ZoomRegion,
    SelectAll,
    Normalize,
    Export,
    /// Remove the selection from the audio (no clipboard).
    Delete,
    /// Write the edited audio back over the open file.
    SaveInPlace,
    /// Step the file cursor in the last-focused pane to the next/previous
    /// sample without leaving the editor.
    NextFile,
    PrevFile,
    /// Add the selection to the end of the slice kit.
    AddSlice,
    /// Show the slice kit / back to the sample.
    ToggleKit,
    SwitchPane,
}

pub fn editor_actions(i: &InputState) -> Vec<EdAction> {
    use EdAction as A;
    let m = i.modifiers;
    let none = !m.any();
    let mut out = Vec::new();
    let mut hit = |k: Key, ok: bool, a: EdAction| {
        if ok && i.key_pressed(k) {
            out.push(a);
        }
    };
    hit(Key::Space, none, A::TogglePlay);
    hit(Key::Space, m.shift_only(), A::TogglePlayAlt);
    hit(Key::S, none, A::ToggleSnap);
    hit(Key::V, none, A::ToggleFollow);
    // `[` / `]` move the in point, with ⇧ (`{` / `}`) the out point. The
    // shifted keys arrive as curly brackets on most layouts.
    let mut zero = Vec::new();
    for (plain, curly, dir) in [(Key::OpenBracket, Key::OpenCurlyBracket, -1i8), (Key::CloseBracket, Key::CloseCurlyBracket, 1)] {
        if !m.command && !m.alt && !m.ctrl {
            if i.key_pressed(curly) || (m.shift && i.key_pressed(plain)) {
                zero.push(A::OutZero(dir));
            } else if !m.shift && i.key_pressed(plain) {
                zero.push(A::InZero(dir));
            }
        }
    }
    hit(Key::C, none, A::Crop);
    hit(Key::C, m.shift_only(), A::Uncrop);
    hit(Key::Z, m.command_only(), A::Undo);
    hit(Key::Z, m.command && m.shift, A::Redo);
    hit(Key::Escape, none, A::Stop);
    hit(Key::I, none, A::SetIn);
    hit(Key::O, none, A::SetOut);
    hit(Key::L, none, A::ToggleLoop);
    hit(Key::M, none, A::ToggleMono);
    for (k, dir) in [(Key::ArrowLeft, -1i8), (Key::ArrowRight, 1)] {
        hit(k, none, A::Nudge(Nudge::Pixel, dir));
        hit(k, m.shift_only(), A::Nudge(Nudge::TenPixels, dir));
        hit(k, m.alt && !m.shift && !m.command, A::Nudge(Nudge::Frame, dir));
    }
    hit(Key::Home, none, A::JumpIn);
    hit(Key::End, none, A::JumpOut);
    hit(Key::Equals, !m.command, A::ZoomIn);
    hit(Key::Plus, !m.command, A::ZoomIn);
    hit(Key::Minus, !m.command, A::ZoomOut);
    hit(Key::F, none, A::ZoomFit);
    hit(Key::Z, none, A::ZoomRegion);
    hit(Key::A, m.command_only(), A::SelectAll);
    hit(Key::N, none, A::Normalize);
    hit(Key::Enter, none, A::Export);
    hit(Key::Backspace, none, A::Delete);
    hit(Key::Delete, none, A::Delete);
    hit(Key::S, m.command_only(), A::SaveInPlace);
    hit(Key::ArrowDown, none, A::NextFile);
    hit(Key::ArrowUp, none, A::PrevFile);
    hit(Key::K, none, A::AddSlice);
    hit(Key::B, none, A::ToggleKit);
    hit(Key::Tab, none, A::SwitchPane);
    out.extend(zero);
    out
}

pub const EDITOR_HELP: &str = "Space play/stop (⇧ from the other start) · I / O set in/out (live while playing) · L loop · M mono on/off · \
S snap to zero · V follow playback · [ / ] step in point (to zero crossings when snapping), ⇧ for out point · C crop to region, ⇧C uncrop · \
⌘Z undo, ⇧⌘Z redo · ←/→ nudge playhead (⇧ ×10, ⌥ 1 frame) · Home/End jump to in/out · +/− zoom · F fit · Z zoom to region · \
⌘X / ⌘C / ⌘V cut / copy / paste (paste goes in at the playhead, or over a selection; works across files) · ⌫ delete selection · ⌘S save in place · \
N normalize · K add selection to slice kit · B show kit / sample · ↩ export · ↑/↓ next/prev sample · Tab next column";
