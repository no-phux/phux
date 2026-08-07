//! In-tree status-bar widget implementations.
//!
//! Each submodule defines a `Widget`-implementing struct plus a
//! `pub(crate) fn factory` that the [`super::WidgetRegistry`] calls.

pub(super) mod cwd;
pub(super) mod exec;
pub(super) mod exit_status;
pub(super) mod help_hints;
pub(super) mod session_name;
pub(super) mod text;
pub(super) mod time;
pub(super) mod windows;
