//! Server-backed host path browse/search, distinct from the spawn-only directory picker.

use std::collections::BTreeMap;

use phux_config::keybind::ResolvedAction;
use phux_protocol::input::key::KeyEvent;
use phux_protocol::input::mouse::MouseEvent;
use phux_protocol::wire::frame::{PathKind, PathQueryResult, PathRow, PathStatus};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::{OverlayCommand, RenderOverlay, SelectItem, SelectList};
use crate::render::{ChromeBreakpoints, Theme};

/// A filterable result list. Nonempty input requests recursive fuzzy search
/// on the host; empty input browses one directory level.
#[derive(Debug)]
pub struct PathPicker {
    root: String,
    list: SelectList,
}

impl PathPicker {
    /// Open a host-backed picker rooted at a directory (empty means home).
    #[must_use]
    pub fn new(root: String, theme: &Theme) -> Self {
        let title = format!("Insert path: {}", if root.is_empty() { "~" } else { &root });
        Self {
            root,
            list: SelectList::new(title, vec![SelectItem::header("Searching host…")], theme)
                .without_vi_navigation(),
        }
    }
}

fn action(name: &str, path: &str) -> ResolvedAction {
    ResolvedAction {
        action: name.to_owned(),
        args: BTreeMap::from([("path".to_owned(), toml::Value::String(path.to_owned()))]),
    }
}

fn rows(result: &PathQueryResult, browsing: bool) -> Vec<SelectItem> {
    match result {
        Err(error) => vec![SelectItem::header(format!(
            "Cannot search {}: {}",
            display(&error.root),
            display(&error.message)
        ))],
        Ok(found) => {
            let mut items = Vec::new();
            if browsing {
                if let Some(parent) = &found.parent {
                    items.push(
                        SelectItem::new("../", action("find-path", parent)).secondary("parent"),
                    );
                }
                items.push(
                    SelectItem::new("./", action("insert-path", &found.root))
                        .secondary("insert this directory"),
                );
            }
            items.extend(found.rows.iter().flat_map(|row| path_items(row, browsing)));
            items.extend(status_note(found.status));
            items
        }
    }
}

fn path_items(row: &PathRow, browsing: bool) -> Vec<SelectItem> {
    let shown = display(&row.path);
    let label = if row.kind == PathKind::Directory {
        format!("{shown}/")
    } else {
        shown.clone()
    };
    let insert = SelectItem::new(label, action("insert-path", &row.path));
    if row.kind != PathKind::Directory || !browsing {
        return vec![insert];
    }
    // Directory navigation is separate: Enter on the insertion row never
    // silently changes the root or starts a process.
    vec![
        SelectItem::new(format!("open {shown}/"), action("find-path", &row.path))
            .secondary("browse"),
        insert.secondary("insert directory"),
    ]
}

/// Host-supplied text as it may be painted: a filename is arbitrary bytes on
/// the host, so a control character is shown escaped, never written to the
/// terminal (L3 §5). The action keeps the original path, which insertion then
/// refuses.
fn display(text: &str) -> String {
    if !text.chars().any(char::is_control) {
        return text.to_owned();
    }
    text.chars()
        .flat_map(|ch| {
            let escaped: Vec<char> = if ch.is_control() {
                ch.escape_default().collect()
            } else {
                vec![ch]
            };
            escaped
        })
        .collect()
}

fn status_note(status: PathStatus) -> Option<SelectItem> {
    let text = match status {
        PathStatus::Complete => return None,
        PathStatus::Warming => "Index warming; refine or retry search",
        PathStatus::Truncated => "Results truncated; refine search",
    };
    Some(SelectItem::header(text))
}

impl RenderOverlay for PathPicker {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.list.render(area, buf);
    }
    fn bounds(&self, area: Rect) -> Option<Rect> {
        self.list.bounds(area)
    }
    fn set_breakpoints(&mut self, bp: ChromeBreakpoints) {
        self.list.set_breakpoints(bp);
    }
    fn set_theme(&mut self, theme: &Theme) {
        self.list.set_theme(theme);
    }
    fn handle_key(&mut self, key: &KeyEvent) -> OverlayCommand {
        let old = self.list.query().to_owned();
        let command = self.list.handle_key(key);
        if self.list.query() != old {
            self.list
                .replace_items(vec![SelectItem::header("Searching host…")]);
        }
        command
    }
    fn handle_paste(&mut self, text: &str) {
        self.list.handle_paste(text);
        self.list
            .replace_items(vec![SelectItem::header("Searching host…")]);
    }
    fn handle_mouse(&mut self, mouse: &MouseEvent) -> OverlayCommand {
        self.list.handle_mouse(mouse)
    }
    fn path_search(&self) -> Option<(&str, &str)> {
        Some((&self.root, self.list.query()))
    }
    fn update_paths(&mut self, result: &PathQueryResult) -> bool {
        self.list
            .replace_items(rows(result, self.list.query().is_empty()));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_protocol::input::key::{KeyAction, ModSet, PhysicalKey};
    use phux_protocol::wire::frame::{PathResults, PathRow};

    #[test]
    fn browse_rows_insert_and_navigate_without_spawning() {
        let result = Ok(PathResults {
            root: "/home".into(),
            parent: Some("/".into()),
            rows: vec![PathRow {
                path: "/home/a b".into(),
                kind: PathKind::Directory,
            }],
            status: PathStatus::Complete,
        });
        let items = rows(&result, true);
        assert_eq!(
            items
                .iter()
                .map(|i| i.action.action.as_str())
                .collect::<Vec<_>>(),
            ["find-path", "insert-path", "find-path", "insert-path"]
        );
        assert_eq!(items[3].action.args["path"].as_str(), Some("/home/a b"));
    }

    #[test]
    fn host_paths_with_controls_are_painted_escaped_and_kept_verbatim() {
        let result = Ok(PathResults {
            root: "/tmp".into(),
            parent: None,
            rows: vec![PathRow {
                path: "/tmp/x\u{1b}]0;pwn\u{7}\ny".into(),
                kind: PathKind::File,
            }],
            status: PathStatus::Complete,
        });
        let items = rows(&result, false);
        assert!(
            !items[0].label.chars().any(char::is_control),
            "{:?}",
            items[0].label
        );
        assert_eq!(
            items[0].action.args["path"].as_str(),
            Some("/tmp/x\u{1b}]0;pwn\u{7}\ny"),
            "the action keeps the original so insertion can refuse it"
        );
        let refused = Err(phux_protocol::wire::frame::PathQueryError {
            root: "/\u{1b}[2J".into(),
            code: phux_protocol::wire::frame::PathErrorCode::Other,
            message: "bad\u{9b}".into(),
        });
        assert!(!rows(&refused, true)[0].label.chars().any(char::is_control));
    }

    #[test]
    fn typing_a_query_clears_old_rows_before_recursive_reply() {
        let result = Ok(PathResults {
            root: "/home".into(),
            parent: Some("/".into()),
            rows: vec![PathRow {
                path: "/home/a".into(),
                kind: PathKind::File,
            }],
            status: PathStatus::Complete,
        });
        let mut picker = PathPicker::new("/home".into(), &Theme::default());
        picker.update_paths(&result);
        let key = KeyEvent {
            action: KeyAction::Press,
            key: PhysicalKey::X,
            mods: ModSet::empty(),
            consumed_mods: ModSet::empty(),
            composing: false,
            text: Some("x".into()),
            unshifted_codepoint: None,
        };
        assert_eq!(picker.handle_key(&key), OverlayCommand::Stay);
        assert_eq!(picker.path_search(), Some(("/home", "x")));
        let enter = KeyEvent {
            key: PhysicalKey::Enter,
            text: None,
            ..key
        };
        assert_eq!(
            picker.handle_key(&enter),
            OverlayCommand::Stay,
            "an old result must not be committed while the host is searching"
        );
    }

    #[test]
    fn the_first_j_or_k_is_search_text_not_vi_navigation() {
        let mut picker = PathPicker::new("/src".into(), &Theme::default());
        let key = KeyEvent {
            action: KeyAction::Press,
            key: PhysicalKey::J,
            mods: ModSet::empty(),
            consumed_mods: ModSet::empty(),
            composing: false,
            text: Some("j".into()),
            unshifted_codepoint: None,
        };
        picker.handle_key(&key);
        assert_eq!(picker.path_search(), Some(("/src", "j")));
    }
}
