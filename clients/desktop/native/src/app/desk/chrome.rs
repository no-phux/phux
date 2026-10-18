//! Window title, focus settling, frame evidence, and sidebar, split and tab drags.

use super::*;

impl Desk {
    /// `{tab} — phux`, or `phux` with no tab, plus the host on a remote window.
    /// Written only when it changes.
    /// The native title names the shown tab and its session, so the Window
    /// menu and Mission Control tell apart windows whose terminals look alike.
    pub(crate) fn sync_title(&mut self, window: &mut Window) {
        let title = match (self.active_tab(), self.session.as_str()) {
            (Some(tab), "") => format!("{} — phux", tab_title(self, tab)),
            (Some(tab), session) => format!("{} — {session}", tab_title(self, tab)),
            (None, _) => "phux".into(),
        };
        let title = match self.remote.as_deref() {
            Some(host) => format!("{title} · {}", display_title(host)),
            None => title,
        };
        if title != self.window_title {
            window.set_window_title(&title);
            self.window_title = title;
        }
    }

    /// Reconcile the keyboard after state changed without a window at hand (a
    /// modal or find bar closed by server activity): nothing focused falls back
    /// to the terminal, or to the window so its key bindings keep working.
    pub(crate) fn settle_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal == Modal::None
            && (self.prepared != Modal::None || window.focused(cx).is_none())
        {
            self.prepared = Modal::None;
            self.modal_inputs.clear();
            self.focus_terminal(window, cx);
        }
    }

    pub(crate) fn dragging_split(&self) -> Option<&str> {
        match self.drag.as_ref()? {
            Drag::Split { id, .. } => Some(id.as_str()),
            _ => None,
        }
    }

    pub(crate) fn dragging_sidebar(&self) -> bool {
        matches!(self.drag, Some(Drag::Sidebar))
    }

    pub(crate) fn dragging_changes(&self) -> bool {
        matches!(self.drag, Some(Drag::Changes))
    }

    pub fn set_viewport(&mut self, size: gpui::Size<gpui::Pixels>) {
        self.viewport = size;
    }

    pub fn frame_report(&self, cx: &gpui::App) -> crate::diag::FrameReport {
        let mut placements = Vec::new();
        let mut painted = Vec::new();
        if let Some(link) = &self.link
            && let Some(tab) = link
                .workspace
                .tabs
                .iter()
                .find(|tab| tab.id == link.workspace.active_id)
        {
            for placement in crate::workspace::layout::placements(&tab.root) {
                placements.push((placement.id.clone(), placement.terminal_label.clone()));
                let Some(entity) = self.panes.get(&placement.id) else {
                    continue;
                };
                let observation = entity.read(cx).painted();
                let bounds = observation.geometry.bounds;
                painted.push(crate::diag::PaintedPane {
                    placement_id: placement.id.clone(),
                    terminal_id: placement.terminal_label.clone(),
                    generation: observation
                        .frame
                        .as_ref()
                        .map_or(0, |frame| frame.generation),
                    bounds,
                    unclipped: pane_unclipped(self.viewport, bounds),
                });
            }
        }
        crate::diag::FrameReport {
            session: self.session.clone(),
            placements,
            painted,
        }
    }

    pub fn begin_sidebar_drag(&mut self, cx: &mut Context<Self>) {
        self.drag = Some(Drag::Sidebar);
        cx.notify();
    }

    pub fn begin_changes_drag(&mut self, cx: &mut Context<Self>) {
        self.drag = Some(Drag::Changes);
        cx.notify();
    }

    pub fn begin_split_drag(&mut self, id: String, axis: Axis, cx: &mut Context<Self>) {
        self.drag = Some(Drag::Split { id, axis });
        cx.notify();
    }

    /// Group the sidebar's sessions, showing the sidebar if it was hidden.
    pub(super) fn group_sidebar(&mut self, grouping: SidebarGrouping, cx: &mut Context<Self>) {
        self.update_prefs(
            |prefs| {
                prefs.sidebar_grouping = grouping;
                prefs.sidebar_visible = true;
            },
            cx,
        );
    }

    /// Whether this window shows the sidebar: wanted, and not withdrawn by
    /// the skin (the plain terminal has none) or the window's role.
    pub(crate) fn sidebar_shown(&self) -> bool {
        self.prefs.sidebar_visible
            && self.role != WindowRole::Quick
            && self.skin().chrome.sidebar > 0.
    }

    /// Where the tabs are painted. Tabs in the sidebar return to the
    /// titlebar while the sidebar is hidden, so they never disappear; quick
    /// windows keep their compact titlebar regardless of the main layout.
    pub(crate) fn tab_position(&self) -> TabPosition {
        if self.sidebar_shown() {
            self.prefs.tab_position
        } else {
            TabPosition::Top
        }
    }

    pub fn begin_tab_drag(&mut self, id: String, x: f32, y: f32, cx: &mut Context<Self>) {
        let position = self.tab_position();
        self.drag = Some(Drag::Tab {
            id,
            position,
            start: tab_coordinate(position, x, y),
            moved: false,
        });
        cx.notify();
    }

    /// The window-wide cursor while a drag is armed.
    pub(crate) fn drag_cursor(&self) -> Option<CursorStyle> {
        match self.drag.as_ref()? {
            Drag::Sidebar
            | Drag::Changes
            | Drag::Split {
                axis: Axis::Row, ..
            } => Some(CursorStyle::ResizeLeftRight),
            Drag::Split {
                axis: Axis::Column, ..
            } => Some(CursorStyle::ResizeUpDown),
            Drag::Tab { moved: true, .. } => Some(CursorStyle::ClosedHand),
            Drag::Tab { .. } => Some(CursorStyle::Arrow),
        }
    }

    /// A window-level pointer move while a drag is armed. A move with no
    /// button held means the release was missed, and ends the drag.
    pub fn drag_move(&mut self, x: f32, y: f32, pressed: bool, cx: &mut Context<Self>) {
        if !pressed {
            self.end_drag(cx);
            return;
        }
        match self.drag.as_mut() {
            Some(Drag::Sidebar) => {
                let width = f64::from(x)
                    .round()
                    .clamp(crate::settings::SIDEBAR_MIN, crate::settings::SIDEBAR_MAX);
                self.prefs.sidebar_width = width;
                cx.notify();
            }
            Some(Drag::Changes) => {
                // The panel hugs the window's right edge, so its width is
                // what lies right of the pointer, leaving the panes room.
                use crate::app::changes::{MIN_WIDTH, PANES_MIN};
                let viewport = self.viewport.width.as_f32();
                let max = (viewport - PANES_MIN).max(MIN_WIDTH);
                self.changes.width = (viewport - x).round().clamp(MIN_WIDTH, max);
                cx.notify();
            }
            Some(Drag::Split { id, axis }) => {
                let (id, axis) = (id.clone(), *axis);
                let Some(bounds) = self.split_bounds.borrow().get(&id).copied() else {
                    return;
                };
                let ratio = match axis {
                    Axis::Row => {
                        (x - bounds.origin.x.as_f32()) / bounds.size.width.as_f32().max(1.)
                    }
                    Axis::Column => {
                        (y - bounds.origin.y.as_f32()) / bounds.size.height.as_f32().max(1.)
                    }
                };
                self.workspace_call(|link| link.workspace.resize_split(&id, ratio));
                cx.notify();
            }
            Some(Drag::Tab {
                id,
                position,
                start,
                moved,
            }) => {
                let coordinate = tab_coordinate(*position, x, y);
                if !*moved && (coordinate - *start).abs() < 6. {
                    return;
                }
                *moved = true;
                let id = id.clone();
                let from = self
                    .link
                    .as_ref()
                    .and_then(|link| link.workspace.tabs.iter().position(|tab| tab.id == id));
                let Some(from) = from else { return };
                let to = self.tab_geometry.borrow().drop_index(from, *position, x, y);
                if to != from {
                    self.workspace_call(|link| link.workspace.move_tab(from, to));
                }
                cx.notify();
            }
            None => {}
        }
    }

    /// Release ends every drag and saves what it changed.
    pub fn end_drag(&mut self, cx: &mut Context<Self>) {
        if self.drag.take().is_some() {
            self.persist();
            cx.notify();
        }
    }

    pub fn new_terminal_in(
        &mut self,
        session_id: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.workspace_call(|link| {
            let cwd = link
                .connection
                .directories
                .get(&session_id)
                .map(String::as_str);
            link.workspace
                .new_terminal_in(&link.connection, session_id, cwd)
        });
        self.after_command(window, cx);
    }
}
