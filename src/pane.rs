//! One side of the window: a role (Source or Destination) plus its sub-tabs.

use crate::tab::DirTab;
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum Role {
    Source,
    Dest,
}

impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Self::Source => "Source",
            Self::Dest => "Destination",
        }
    }

    pub fn other(self) -> Self {
        match self {
            Self::Source => Self::Dest,
            Self::Dest => Self::Source,
        }
    }
}

pub struct Pane {
    pub role: Role,
    pub tabs: Vec<DirTab>,
    pub active: usize,
    /// Id of the local tab last active, to fall back to when the device goes.
    pub last_local: Option<u64>,
}

impl Pane {
    pub fn active_tab(&self) -> &DirTab {
        &self.tabs[self.active.min(self.tabs.len() - 1)]
    }

    pub fn active_tab_mut(&mut self) -> &mut DirTab {
        let i = self.active.min(self.tabs.len() - 1);
        &mut self.tabs[i]
    }

    pub fn add_tab(&mut self, tab: DirTab) {
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
    }

    /// Closing the last tab is a no-op -- a pane with no tabs has nothing to
    /// show and no directory to commit into.
    pub fn close_tab(&mut self, idx: usize) {
        if self.tabs.len() <= 1 || idx >= self.tabs.len() {
            return;
        }
        self.tabs.remove(idx);
        // Keep the same tab active when one to its left goes away.
        if idx < self.active {
            self.active -= 1;
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
    }

    /// Remove a tab and hand it back (to move it to the other pane). Like
    /// `close_tab`, refuses to take the last one.
    pub fn take_tab(&mut self, idx: usize) -> Option<DirTab> {
        if self.tabs.len() <= 1 || idx >= self.tabs.len() {
            return None;
        }
        let tab = self.tabs.remove(idx);
        if idx < self.active {
            self.active -= 1;
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
        Some(tab)
    }

    /// Close every tab except `idx`, which becomes the active one.
    pub fn close_other_tabs(&mut self, idx: usize) {
        if idx >= self.tabs.len() {
            return;
        }
        let keep = self.tabs.swap_remove(idx);
        self.tabs = vec![keep];
        self.active = 0;
    }

    pub fn cycle(&mut self, delta: isize) {
        let n = self.tabs.len() as isize;
        self.active = ((self.active as isize + delta).rem_euclid(n)) as usize;
    }

    /// Remember the active tab if it is a local one.
    pub fn note_active(&mut self) {
        let t = self.active_tab();
        if !t.device {
            self.last_local = Some(t.id);
        }
    }

    /// If a device tab is showing, switch to the last local tab (or any
    /// local one). Returns false when there is no local tab to go to.
    pub fn leave_device(&mut self) -> bool {
        if !self.active_tab().device {
            return true;
        }
        let by_id = self
            .last_local
            .and_then(|id| self.tabs.iter().position(|t| t.id == id && !t.device));
        match by_id.or_else(|| self.tabs.iter().position(|t| !t.device)) {
            Some(i) => {
                self.active = i;
                true
            }
            None => false,
        }
    }

    pub fn tab_dirs(&self) -> Vec<PathBuf> {
        self.tabs.iter().map(|t| t.cwd.clone()).collect()
    }
}
