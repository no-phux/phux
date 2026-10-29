//! Palettes, the path picker, dialogs, and the find bar. Lists are the Kit
//! [`Command`](gpui_kit::component::command::Command) palette; text entry is
//! Kit [`Input`](gpui_kit::component::input::Input). Every modal is a
//! [`sheet`] on one backdrop: the same placement, a click outside or Escape
//! closes it, a click inside never does, and its control takes the keyboard
//! on open.

mod dialogs;
mod find_bar;
mod goto_facts;
mod goto_filter;
mod goto_frecency;
mod goto_search;
mod inputs;
pub(crate) mod launcher;
mod notifications;
mod path_panel;
mod picking;
mod ranking;
mod rows;
mod surface;
mod theme_panel;
mod windows;

pub use find_bar::find_bar;
pub(crate) use goto_filter::GotoPalette;
pub(crate) use goto_frecency::Store as GotoFrecency;
pub(crate) use goto_search::GotoSearch;
pub(crate) use inputs::{find_input, prepare};
pub(crate) use path_panel::{PathItem, path_items};
pub use surface::layer;
pub(crate) use surface::{SHEET_GUTTER, card, corner, line_height, sheet};

use picking::Target;
use ranking::{filter_sections, goto_id, prepare_goto, query_terms, rank_sections, section_ids};
use rows::{
    CURRENT, Row, Section, command_results, command_sections, current_mark, goto_sections,
    settings_sections, simple_row, switcher_sections,
};
use surface::inset_radius;
use windows::{WINDOWS, move_sections, raise_window, session_sections, window_sections};

#[cfg(test)]
use ranking::{goto_candidates, search_revision};

#[cfg(test)]
pub(super) mod palette_tests;
#[cfg(test)]
mod polish_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod window_tests;

/// Transient disclosure state, reset whenever a palette opens.
#[derive(Default)]
pub(super) struct PaletteView {
    all_commands: bool,
    details: bool,
    pub(crate) edit: Option<crate::app::settings_panel::catalog::Key>,
    pub(crate) edit_focus: Option<gpui::FocusHandle>,
}

/// Text fields a modal or the find bar owns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputSlot {
    Find,
    Rename,
    SessionName,
    SessionDir,
    HostTarget,
    HostName,
    Project,
}
