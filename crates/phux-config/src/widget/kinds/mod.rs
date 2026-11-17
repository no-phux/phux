//! Per-kind option specs and validators for the built-in status-bar
//! widgets.
//!
//! Each module owns one kind's [`super::WidgetKindSpec`] and a pure `parse`
//! that turns the kind-specific options (universal ones already stripped)
//! into resolved values. `phux config check` runs the same `parse` the TUI's
//! factories build from, so the checked surface and the rendered one cannot
//! drift.

pub mod button;
pub mod cwd;
pub mod exec;
pub mod exit_status;
pub mod help_hints;
pub mod session_name;
pub mod spacer;
pub mod switch;
pub mod text;
pub mod time;
pub mod windows;
