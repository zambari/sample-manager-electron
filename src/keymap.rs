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
    /// Audition the sample under the cursor.
    Preview,
    /// Begin inline rename.
    Rename,
    /// Descend into the directory under the cursor.
    OpenDir,
    GoUp,
    /// Start a recursive size walk for the selection.
    ComputeSize,
    Trash,
    SelectAll,
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
            (plain(Key::Space), Action::Preview),
            (plain(Key::ArrowUp), Action::CursorUp),
            (plain(Key::ArrowDown), Action::CursorDown),
            (shift(Key::ArrowUp), Action::ExtendUp),
            (shift(Key::ArrowDown), Action::ExtendDown),
            (plain(Key::ArrowRight), Action::OpenDir),
            (plain(Key::ArrowLeft), Action::GoUp),
            (plain(Key::Escape), Action::Cancel),
            (cmd(Key::A), Action::SelectAll),
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
            (cmd(Key::Enter), Action::Rename),
            (cmd(Key::ArrowDown), Action::OpenDir),
            (cmd(Key::ArrowUp), Action::GoUp),
            (cmd(Key::Backspace), Action::Trash),
            (cmd(Key::CloseBracket), Action::NextTab),
            (cmd(Key::OpenBracket), Action::PrevTab),
        ]);
        Self { name: "macOS", bindings }
    }

    pub fn windows() -> Self {
        let mut bindings = Self::common();
        bindings.extend([
            (plain(Key::F2), Action::Rename),
            (plain(Key::F5), Action::Refresh),
            (plain(Key::Backspace), Action::GoUp),
            (plain(Key::Delete), Action::Trash),
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
    pub fn actions(&self, i: &InputState) -> Vec<Action> {
        let mut hits: Vec<(usize, Action)> = Vec::new();
        for (chord, action) in &self.bindings {
            if i.key_pressed(chord.key) && i.modifiers == chord.mods {
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
