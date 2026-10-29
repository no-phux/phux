//! Find in terminal and the Insert Path picker.

use super::*;

impl Desk {
    /// Open find on the focused pane with its field focused; reopening re-runs the search.
    pub(super) fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(placement_id) = self.focused_id() else {
            return;
        };
        if let Some(find) = self
            .find
            .as_ref()
            .filter(|find| find.placement_id == placement_id)
        {
            let (query, case) = (find.query.clone(), find.case_sensitive);
            self.run_search(query, case, cx);
            if self.find_input.is_none() {
                self.find_input = Some(super::super::overlays::find_input("", window, cx));
            }
        }
        self.close_find(cx);
        self.find = Some(FindState {
            placement_id,
            query: String::new(),
            case_sensitive: false,
            matches: Vec::new(),
            index: 0,
        });
        self.find_input = Some(super::super::overlays::find_input("", window, cx));
    }

    /// Search for the first selected line, and show it in the field.
    pub(super) fn find_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(placement_id) = self.focused_id() else {
            return;
        };
        let selected = self.selection_text().unwrap_or_default();
        let query = selected
            .split(['\n', '\r'])
            .find(|line| !line.trim().is_empty())
            .unwrap_or("")
            .chars()
            .take(512)
            .collect::<String>();
        if query.is_empty() {
            self.open_find(window, cx);
            return;
        }
        let case = self.find.as_ref().is_some_and(|find| find.case_sensitive);
        if self
            .find
            .as_ref()
            .is_some_and(|find| find.placement_id != placement_id)
        {
            self.close_find(cx);
        }
        self.find = Some(FindState {
            placement_id,
            query: query.clone(),
            case_sensitive: case,
            ..Default::default()
        });
        match self.find_input.as_ref() {
            Some((input, _)) => {
                let input = input.clone();
                input.update(cx, |input, cx| input.set_value(query.clone(), window, cx));
            }
            None => self.find_input = Some(super::super::overlays::find_input(&query, window, cx)),
        }
        self.run_search(query, case, cx);
    }

    pub fn run_search(&mut self, query: String, case_sensitive: bool, cx: &mut Context<Self>) {
        let Some(state) = self.find.clone() else {
            return;
        };
        let Some(placement) = self.placement(&state.placement_id).cloned() else {
            return;
        };
        let Some(view) = placement.view_id else {
            return;
        };
        let matches = if query.is_empty() {
            Vec::new()
        } else {
            self.client()
                .and_then(|client| client.engine())
                .and_then(|engine| engine.search_view(view, query.clone(), case_sensitive).ok())
                .unwrap_or_default()
                .into_iter()
                .map(|found| (found.start, found.end))
                .collect()
        };
        self.show_match(&placement, &matches, 0, cx);
        self.find = Some(FindState {
            query,
            case_sensitive,
            matches,
            index: 0,
            ..state
        });
    }

    /// Next or previous match, wrapping. A step with no matches searches again.
    pub(crate) fn step_find(&mut self, delta: i32, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.find.clone() else {
            self.open_find(window, cx);
            return;
        };
        if state.matches.is_empty() {
            self.run_search(state.query, state.case_sensitive, cx);
            return;
        }
        let len = state.matches.len() as i32;
        let index = (state.index as i32 + delta).rem_euclid(len) as usize;
        if let Some(placement) = self.placement(&state.placement_id).cloned() {
            self.show_match(&placement, &state.matches, index, cx);
        }
        self.find = Some(FindState { index, ..state });
    }

    pub(super) fn show_match(
        &self,
        placement: &Placement,
        matches: &[(u64, u64)],
        index: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = placement.view_id else {
            return;
        };
        let Some(client) = self.client() else { return };
        let Some(engine) = client.engine() else {
            return;
        };
        if let Some((start, end)) = matches.get(index) {
            let _ = engine.set_view_selection(view, *start, *end, false);
            let _ = client.pin_viewport_view(view, *start);
        } else {
            let _ = engine.clear_view_selection(view);
        }
        self.repaint_placement(&placement.id, cx);
    }

    pub(super) fn close_find(&mut self, cx: &mut Context<Self>) {
        self.find_input = None;
        if let Some(state) = self.find.take()
            && let Some(placement) = self.placement(&state.placement_id).cloned()
            && let Some(view) = placement.view_id
            && let Some(engine) = self.client().and_then(|client| client.engine())
        {
            let _ = engine.clear_view_selection(view);
            self.repaint_placement(&placement.id, cx);
        }
    }

    pub fn find_status(&self) -> String {
        let Some(state) = &self.find else {
            return String::new();
        };
        if state.query.is_empty() {
            return String::new();
        }
        if state.matches.is_empty() {
            return "no matches".into();
        }
        format!(
            "{} of {}{}",
            state.index + 1,
            state.matches.len(),
            if state.matches.len() >= 4096 { "+" } else { "" }
        )
    }

    pub(super) fn open_path_picker(&mut self) {
        let target = self.path_target();
        let features = self
            .link
            .as_ref()
            .map(|link| link.connection.features())
            .unwrap_or_default();
        let attached = self.session_is_attached();
        let Some(target) = target.filter(|target| {
            paths::can_offer_path_picker(Some(features.as_slice()), attached, Some(target))
        }) else {
            let body = if attached && self.path_target().is_some() {
                "This server can't search paths. Update phux on that machine."
            } else {
                "Not attached to a terminal."
            };
            self.post(
                Kind::Refusal,
                "Insert Path is unavailable",
                Some(body.into()),
            );
            return;
        };
        let root = self
            .pane_cwd(crate::connection::parse_terminal(&target.terminal_id).as_ref())
            .unwrap_or_else(|| "~".into());
        self.picker = Some(paths::begin_picker(target, root.clone()));
        self.modal = Modal::Path;
        self.query_paths(root, String::new());
    }

    pub fn query_paths(&mut self, root: String, query: String) {
        let Some(active) = self.picker.clone() else {
            return;
        };
        let Some(current) = self
            .path_target()
            .filter(|current| paths::same_target(&active.target, Some(current)))
        else {
            self.close_path_picker();
            return;
        };
        let host = paths::host_for_terminal(&current.terminal_id).map(SatelliteHost::new);
        let id = self.link.as_ref().map_or(0, |link| {
            link.connection
                .path_query(root.clone(), query.clone(), !query.is_empty(), host)
        });
        self.picker = Some(if id == 0 {
            let mut failed = active;
            failed.root = root;
            failed.query = query;
            paths::fail_search(failed, "Path search is unavailable on this server.")
        } else {
            paths::start_query(active, id, root, &query)
        });
    }

    /// Paste the quoted path into the picker's pane. The picker stays open with
    /// the reason when nothing was queued.
    pub fn insert_path(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(active) = self.picker.clone() else {
            return;
        };
        let current = self.path_target();
        let ready = current
            .as_ref()
            .and_then(|target| crate::connection::parse_terminal(&target.terminal_id))
            .is_some_and(|id| {
                self.link
                    .as_ref()
                    .is_some_and(|link| link.connection.ready(&id))
            });
        let Some(text) = paths::prepared_insertion(Some(&active), &path, current.as_ref(), ready)
        else {
            let mut rejected = active;
            rejected.failure =
                Some("The pane changed or is not ready; nothing was inserted.".into());
            self.picker = Some(rejected);
            cx.notify();
            return;
        };
        if self.paste_text(&text) {
            self.close_modal(window, cx);
            self.after_command(window, cx);
        } else if let Some(picker) = self.picker.as_mut() {
            picker.failure = Some("The path was not queued; nothing was inserted.".into());
            cx.notify();
        }
    }

    pub fn open_path_terminal(
        &mut self,
        directory: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.workspace_call(|link| {
            link.workspace
                .new_terminal(&link.connection, Some(directory.as_str()))
        });
        self.close_modal(window, cx);
        self.after_command(window, cx);
    }

    /// Browse into a directory: it becomes the root and the query clears.
    pub(crate) fn browse_path(
        &mut self,
        directory: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = self.palette_state.clone() {
            state.update(cx, |state, cx| state.set_query("", window, cx));
        }
        self.query_paths(directory, String::new());
        cx.notify();
    }

    /// Tab or Right on a directory row (or the parent row) browses into it.
    pub(crate) fn open_selected_directory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.palette_state.clone() else {
            return;
        };
        let Some(row) = state.read(cx).selected_index().map(|index| index.row) else {
            return;
        };
        let directory = super::super::overlays::path_items(self)
            .into_iter()
            .nth(row)
            .and_then(|item| item.directory());
        if let Some(directory) = directory {
            self.browse_path(directory, window, cx);
        }
    }

    pub(super) fn close_path_picker(&mut self) {
        self.picker = None;
        if matches!(self.modal, Modal::Path) {
            self.modal = Modal::None;
        }
    }

    /// The focused pane's cursor cell, where the path picker opens.
    pub(crate) fn cursor_anchor(&self, cx: &gpui::App) -> Option<gpui::Bounds<gpui::Pixels>> {
        let placement = self.focused_placement()?;
        self.panes.get(&placement.id)?.read(cx).cursor_bounds(cx)
    }

    pub(crate) fn path_target(&self) -> Option<paths::PathTarget> {
        let placement = self.focused_placement()?;
        let link = self.link.as_ref()?;
        let server = link.connection.server_id()?;
        Some(paths::PathTarget {
            terminal_id: placement.terminal_label.clone(),
            placement_id: placement.id.clone(),
            server_id: server,
            epoch: link.connection.connection_epoch().to_string(),
        })
    }
}
