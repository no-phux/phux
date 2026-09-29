//! The chrome a paint writes, handed to the paint layer as one value.
//!
//! **TL;DR.** [`ChromeCtx`] bundles the six things every composited frame
//! threads to its chrome tail: the viewport, the sidebar reservation, the two
//! chrome painters, the session name, and the theme. [`PaneScene`] is the
//! shared view of where the panes are that the chrome projection reads. Both
//! are built at the call site for one paint and dropped after it.
//!
//! The long-lived owner of this state is the driver's `SessionLoop` (with
//! `TuiSettings` holding the bar and theme); these types exist so the paint
//! functions take one context instead of the same six positional arguments
//! each (phux-jx39.1). Repaint POLICY is not here: it lives in
//! [`super::repaint`].

use std::collections::HashMap;

use phux_protocol::ids::ResourceId;

use super::paint::{ContentLayout, SidebarReservation, content_layout};
use super::pane_state::PaneSlot;
use crate::layout::Workspace;
use crate::render::Theme;
use crate::render::chrome::sidebar::SidebarPainter;
use crate::render::chrome::status_bar::{Position, StatusBarPainter};
use crate::settings::TuiSettings;

/// The chrome of one composited frame.
///
/// `sidebar_painter` stays optional because a paint without it deliberately
/// leaves the strip's columns blank (the sidebar-visibility tests pin that).
pub(super) struct ChromeCtx<'a> {
    /// The outer terminal viewport, `(cols, rows)`.
    pub(super) viewport: (u16, u16),
    /// This frame's sidebar reservation; `None` keeps the full width.
    pub(super) sidebar: Option<SidebarReservation>,
    /// The status-bar painter, or `None` for a bar-less config.
    pub(super) status_bar: Option<&'a mut StatusBarPainter>,
    /// The window-strip painter, or `None` to leave the strip unpainted.
    pub(super) sidebar_painter: Option<&'a mut SidebarPainter>,
    /// The attached session's name, as the bar renders it.
    pub(super) session_name: &'a str,
    /// Chrome and overlay colors.
    pub(super) theme: &'a Theme,
}

impl<'a> ChromeCtx<'a> {
    /// Borrow the chrome out of the loop's settings and painters.
    pub(super) const fn new(
        settings: &'a mut TuiSettings,
        sidebar_painter: &'a mut SidebarPainter,
        session_name: &'a str,
        viewport: (u16, u16),
        sidebar: Option<SidebarReservation>,
    ) -> Self {
        Self {
            viewport,
            sidebar,
            status_bar: settings.status_bar.as_mut(),
            sidebar_painter: Some(sidebar_painter),
            session_name,
            theme: &settings.theme,
        }
    }

    /// The row the status bar docks to, if there is a bar.
    pub(super) fn bar(&self) -> Option<Position> {
        self.status_bar.as_deref().map(StatusBarPainter::position)
    }

    /// The pane area and rail row this frame's chrome leaves.
    pub(super) fn content_layout(&self) -> ContentLayout {
        content_layout(self.viewport, self.bar(), self.sidebar)
    }
}

/// Where the panes are and which one is focused: the shared view the chrome
/// projection reads. No kernel, so a chrome-only caller needs no replica.
#[derive(Clone, Copy)]
pub(super) struct PaneScene<'a> {
    /// The layout mirror.
    pub(super) workspace: &'a Workspace,
    /// The client-local pane slots.
    pub(super) panes: &'a HashMap<ResourceId, PaneSlot>,
    /// The focused leaf, when there is one.
    pub(super) focused: Option<&'a ResourceId>,
    /// The zoomed leaf, when a pane fills its window.
    pub(super) zoomed: Option<&'a ResourceId>,
}
