//! The Elek Project Editor (experimental; DT1 and DT2, DT2 less tested): the loaded
//! project's 128 patterns as one long list, filtered by bank. Each row folds
//! out into a dot view of its tracks' steps. Move, copy, clear and rename
//! whole patterns, or copy single tracks into other patterns; the result goes to the device with the same push as the
//! sample order, so both pages edit one loaded project.
//! Page controls, state, clipboards, editing and rendering live in the sibling modules.

use crate::device::library::{PoolSound, SoundCell, SoundSrc};
use crate::device::patterns::{self, Patterns, Step, TrackSeq, NAME_MAX, PATTERNS};
use crate::ui::elek_library_view::{is_default, machine_text, param_at, src_detail, SRC_SHOWN};
use crate::device::{Cell, ElekProject, PatState};
use crate::ui::elek_view::{self, ElekIntent, ElekState};
use crate::ui::sound_tags;
use crate::ui::theme::{self, oled_font, OLED, OLED_BG, OLED_DIM, OLED_HOVER};
use egui::{Color32, Rect, RichText, Sense, Stroke};
use std::collections::BTreeSet;

const BANKS: usize = 8;
const PER_BANK: usize = 16;
const ROW_H: f32 = 26.0;
const TRACK_H: f32 = 15.0;
const BANK_H: f32 = 20.0;
/// Where the name column starts, and its width.
const NAME_X: f32 = 84.0;
const NAME_W: f32 = 170.0;
/// Where the step dots start, in folded-out rows.
const DOTS_X: f32 = 370.0;
const WARN: Color32 = Color32::from_rgb(230, 120, 90);
/// Outline of a selected pattern row (no fill, so the row stays readable).
const SEL_EDGE: Color32 = Color32::from_rgb(190, 104, 40);
const ROW_FILL: Color32 = Color32::from_rgb(18, 21, 21);
const DOT_OFF: Color32 = Color32::from_rgb(38, 45, 44);
const DOT_BEAT: Color32 = Color32::from_rgb(62, 72, 70);
/// A selected track line in a folded-out row.
const TRACK_SEL: Color32 = Color32::from_rgb(110, 58, 18);

mod clipboard;
mod controls;
mod edit;
mod list;
mod rows;
mod sounds;
mod state;
mod steps;

pub use clipboard::PoolClip;
pub use controls::show;
pub(crate) use controls::view_tools;
pub use state::ProjEd;

use clipboard::{TrackPaste, TrackClip, Part, copy_tracks,
    paste_tracks, apply_tracks, request_sound_paste, pool_cell_sound, finish_sound_actions,
    copy_items, paste_items, confirm_tracks};
use edit::{drop_sel, clear, copy, paste, pack, rename};
use list::{list, ListActions};
use rows::{shown_tracks, bank_header, track_row, TrackRow, pattern_menu, PatternMenu};
use sounds::{sound_foldout, SoundFold};
use state::{view, seq_of, track_name, blank_seq, bank_letter};
use steps::{hex_row, nearest_step, len_label, pattern_scale, timing_tip, step_tip,
    dots, mini, triangle};
