//! The picker's rows: a host path query's answer as rows to insert or
//! browse, and the notes (the search status, errors) that commit nothing.

use std::collections::BTreeMap;

use phux_config::keybind::ResolvedAction;
use phux_protocol::wire::frame::{PathKind, PathQueryResult, PathRow, PathStatus};

/// One row of the picker: a path to insert or browse, or a note (the
/// search status, an error) that commits nothing.
#[derive(Debug, Clone)]
pub(super) struct Row {
    pub(super) label: String,
    /// A dimmed, right-aligned description (`browse`, `parent`).
    pub(super) secondary: Option<String>,
    /// What choosing the row runs; `None` for a note.
    pub(super) action: Option<ResolvedAction>,
}

impl Row {
    pub(super) fn new(label: impl Into<String>, action: ResolvedAction) -> Self {
        Self {
            label: label.into(),
            secondary: None,
            action: Some(action),
        }
    }

    pub(super) fn note(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            secondary: None,
            action: None,
        }
    }

    pub(super) fn secondary(mut self, secondary: impl Into<String>) -> Self {
        self.secondary = Some(secondary.into());
        self
    }
}

fn action(name: &str, path: &str) -> ResolvedAction {
    crate::attach::resolved(
        name,
        BTreeMap::from([("path".to_owned(), toml::Value::String(path.to_owned()))]),
    )
}

pub(super) fn rows(result: &PathQueryResult, browsing: bool) -> Vec<Row> {
    match result {
        Err(error) => vec![Row::note(format!(
            "Cannot search {}: {}",
            display(&error.root),
            display(&error.message)
        ))],
        Ok(found) => {
            let mut items = Vec::new();
            if browsing {
                if let Some(parent) = &found.parent {
                    items.push(Row::new("../", action("find-path", parent)).secondary("parent"));
                }
                items.push(
                    Row::new("./", action("insert-path", &found.root))
                        .secondary("insert this directory"),
                );
            }
            items.extend(
                found
                    .rows
                    .iter()
                    .flat_map(|row| path_items(row, &found.root, browsing)),
            );
            items.extend(status_note(found.status));
            items
        }
    }
}

fn path_items(row: &PathRow, root: &str, browsing: bool) -> Vec<Row> {
    let shown = display(relative(&row.path, root));
    let label = if row.kind == PathKind::Directory {
        format!("{shown}/")
    } else {
        shown.clone()
    };
    let insert = Row::new(label, action("insert-path", &row.path));
    if row.kind != PathKind::Directory || !browsing {
        return vec![insert];
    }
    // Directory navigation is separate: Enter on the insertion row never
    // silently changes the root or starts a process.
    vec![
        Row::new(format!("open {shown}/"), action("find-path", &row.path)).secondary("browse"),
        insert.secondary("insert directory"),
    ]
}

/// How a row names `path`: relative to the `root` it was found under, so
/// rows read as names rather than one shared prefix cut at the box edge.
pub(super) fn relative<'a>(path: &'a str, root: &str) -> &'a str {
    path.strip_prefix(root)
        .and_then(|rest| {
            if root.ends_with('/') {
                Some(rest)
            } else {
                rest.strip_prefix('/')
            }
        })
        .filter(|rest| !rest.is_empty())
        .unwrap_or(path)
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

fn status_note(status: PathStatus) -> Option<Row> {
    let text = match status {
        PathStatus::Complete => return None,
        PathStatus::Warming => "Index warming; refine or retry search",
        PathStatus::Truncated => "Results truncated; refine search",
    };
    Some(Row::note(text))
}
