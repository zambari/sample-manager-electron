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
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
    }

    pub fn cycle(&mut self, delta: isize) {
        let n = self.tabs.len() as isize;
        self.active = ((self.active as isize + delta).rem_euclid(n)) as usize;
    }

    pub fn tab_dirs(&self) -> Vec<PathBuf> {
        self.tabs.iter().map(|t| t.cwd.clone()).collect()
    }
}
