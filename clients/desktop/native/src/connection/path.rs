//! Host path picker. Results are paths on the server host, never on this machine.
//! Rows stay in the server's order. A search reply does not grant paste admission.

use phux_client_runtime::control::{PathAnswer, PathFailure, PathMatchKind, PathSearchStatus};

/// One row the picker shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathRow {
    /// Absolute host path.
    pub path: String,
    /// `file`, `directory`, or `symlink`.
    pub kind: String,
}

/// The pane a search belongs to. A replaced placement, server, or epoch cannot insert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathTarget {
    /// Terminal spelling.
    pub terminal_id: String,
    /// Placement id.
    pub placement_id: String,
    /// Hex server incarnation.
    pub server_id: String,
    /// Connection epoch, decimal.
    pub epoch: String,
}

/// Picker state between searches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerState {
    /// Admission target.
    pub target: PathTarget,
    /// Current root.
    pub root: String,
    /// Query text.
    pub query: String,
    /// Server rows, unranked.
    pub rows: Vec<PathRow>,
    /// Lexical parent, when the answer named one.
    pub parent: Option<String>,
    /// Outstanding request. Late replies for another id are ignored.
    pub pending: Option<u32>,
    /// Status line.
    pub message: String,
}

/// Start a picker at `root`.
#[must_use]
pub fn begin_picker(target: PathTarget, root: impl Into<String>) -> PickerState {
    PickerState {
        target,
        root: root.into(),
        query: String::new(),
        rows: Vec::new(),
        parent: None,
        pending: None,
        message: String::new(),
    }
}

/// The server advertised path query, the client is attached, and a target exists.
#[must_use]
pub fn can_offer_path_picker(
    features: Option<&[String]>,
    attached: bool,
    target: Option<&PathTarget>,
) -> bool {
    attached
        && target.is_some()
        && features.is_some_and(|features| features.iter().any(|feature| feature == "path-query"))
}

/// A new search replaces the listing until its own reply arrives. It searches beneath `root`.
#[must_use]
pub fn start_query(
    mut state: PickerState,
    request_id: u32,
    root: impl Into<String>,
    query: impl Into<String>,
) -> PickerState {
    state.root = root.into();
    state.query = query.into();
    state.pending = Some(request_id);
    state.rows.clear();
    state.message = "Searching host paths…".to_owned();
    state
}

/// Apply one answer. A stale request id leaves the state unchanged.
#[must_use]
pub fn accept_path_answer(state: Option<PickerState>, reply: &PathAnswer) -> Option<PickerState> {
    let state = state?;
    if state.pending != Some(reply.request_id) {
        return Some(state);
    }
    if let Some(failure) = reply.failure {
        return Some(PickerState {
            pending: None,
            rows: Vec::new(),
            message: if reply.message.is_empty() {
                failure_text(failure).to_owned()
            } else {
                reply.message.clone()
            },
            ..state
        });
    }
    Some(PickerState {
        pending: None,
        root: reply.root.clone(),
        parent: reply.parent.clone(),
        rows: reply
            .rows
            .iter()
            .map(|row| PathRow {
                path: row.path.clone(),
                kind: kind_name(row.kind).to_owned(),
            })
            .collect(),
        message: match reply.status {
            Some(PathSearchStatus::Truncated) => "Results truncated; narrow the search.".to_owned(),
            Some(PathSearchStatus::Warming) => {
                "Host index is warming; search again shortly.".to_owned()
            }
            _ => String::new(),
        },
        ..state
    })
}

fn failure_text(failure: PathFailure) -> &'static str {
    match failure {
        PathFailure::NotFound => "not found",
        PathFailure::PermissionDenied => "permission denied",
        PathFailure::NotADirectory => "not a directory",
        PathFailure::Unanswered => "unanswered",
        PathFailure::Other => "path query failed",
    }
}

fn kind_name(kind: PathMatchKind) -> &'static str {
    match kind {
        PathMatchKind::File => "file",
        PathMatchKind::Directory => "directory",
        PathMatchKind::Symlink => "symlink",
    }
}

/// Same pane, placement, server, and epoch.
#[must_use]
pub fn same_target(target: &PathTarget, current: Option<&PathTarget>) -> bool {
    current.is_some_and(|current| {
        current.terminal_id == target.terminal_id
            && current.placement_id == target.placement_id
            && current.server_id == target.server_id
            && current.epoch == target.epoch
    })
}

/// A quoted path ready to paste, or `None` when admission fails.
///
/// The row must be one this search returned, the target must still match, and
/// no search may be outstanding. Readiness is checked again here.
#[must_use]
pub fn prepared_insertion(
    state: Option<&PickerState>,
    path: &str,
    current: Option<&PathTarget>,
    ready: bool,
) -> Option<String> {
    let state = state?;
    if state.pending.is_some() || !ready || !same_target(&state.target, current) {
        return None;
    }
    if !state.rows.iter().any(|row| row.path == path) {
        return None;
    }
    shell_quote_path(path)
}

/// A single POSIX-shell word. Not a command, and not a line ending in Enter.
#[must_use]
pub fn shell_quote_path(path: &str) -> Option<String> {
    if !path.starts_with('/') || path.chars().any(is_terminal_control) {
        return None;
    }
    Some(format!("'{}'", path.replace('\'', r#"'"'"'"#)))
}

/// Host text as the picker shows it. Controls are escaped, never rendered raw.
#[must_use]
pub fn display_path(path: &str) -> String {
    let mut shown = String::new();
    for character in path.chars() {
        if is_terminal_control(character) {
            shown.push_str(&format!("\\u{{{:x}}}", character as u32));
        } else {
            shown.push(character);
        }
    }
    shown
}

fn is_terminal_control(character: char) -> bool {
    let code = character as u32;
    code < 0x20 || (0x7f..=0x9f).contains(&code)
}

/// `satellite:<host>:<id>` host, including hosts that contain colons.
#[must_use]
pub fn host_for_terminal(terminal_id: &str) -> Option<String> {
    if terminal_id.starts_with("local:") {
        return None;
    }
    let raw = terminal_id.strip_prefix("satellite:")?;
    let (host, id) = raw.rsplit_once(':')?;
    (id.chars().all(|ch| ch.is_ascii_digit()) && !host.is_empty()).then(|| host.to_owned())
}

/// A host directory the path picker can start a terminal in. Files insert only.
#[must_use]
pub fn directory_for_terminal(kind: &str, path: &str) -> Option<String> {
    if kind != "directory" {
        return None;
    }
    let directory = path.trim();
    (!directory.is_empty()).then(|| directory.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_client_runtime::control::{PathAnswer, PathMatch, PathMatchKind, PathSearchStatus};

    fn target() -> PathTarget {
        PathTarget {
            terminal_id: "local:1".into(),
            placement_id: "place-1".into(),
            server_id: "a".into(),
            epoch: "3".into(),
        }
    }

    #[test]
    fn offer_requires_the_feature_an_attach_and_a_target() {
        let target = target();
        assert!(!can_offer_path_picker(None, true, Some(&target)));
        assert!(!can_offer_path_picker(Some(&[]), true, Some(&target)));
        assert!(!can_offer_path_picker(
            Some(&["path-query".into()]),
            false,
            Some(&target)
        ));
        assert!(!can_offer_path_picker(
            Some(&["path-query".into()]),
            true,
            None
        ));
        assert!(can_offer_path_picker(
            Some(&["path-query".into()]),
            true,
            Some(&target)
        ));
    }

    #[test]
    fn stale_replies_and_replaced_targets_do_not_insert() {
        let searching = start_query(begin_picker(target(), "~"), 10, "/repo", "main");
        assert_eq!(searching.root, "/repo");
        let stale = PathAnswer {
            request_id: 9,
            root: "/host".into(),
            parent: None,
            rows: vec![PathMatch {
                path: "/host/old".into(),
                kind: PathMatchKind::File,
            }],
            status: Some(PathSearchStatus::Complete),
            failure: None,
            message: String::new(),
        };
        assert_eq!(
            accept_path_answer(Some(searching.clone()), &stale)
                .unwrap()
                .query,
            "main"
        );
        let found = accept_path_answer(
            Some(searching),
            &PathAnswer {
                request_id: 10,
                root: "/host".into(),
                parent: None,
                rows: vec![PathMatch {
                    path: "/host/it's here".into(),
                    kind: PathMatchKind::File,
                }],
                status: Some(PathSearchStatus::Complete),
                failure: None,
                message: String::new(),
            },
        );
        assert_eq!(
            prepared_insertion(found.as_ref(), "/host/it's here", Some(&target()), true).as_deref(),
            Some("'/host/it'\"'\"'s here'")
        );
        assert!(prepared_insertion(found.as_ref(), "/host/other", Some(&target()), true).is_none());
        let mut replaced = target();
        replaced.epoch = "4".into();
        assert!(
            prepared_insertion(found.as_ref(), "/host/it's here", Some(&replaced), true).is_none()
        );
        assert!(shell_quote_path("/host/evil\ncommand").is_none());
        assert!(shell_quote_path("relative").is_none());
        assert_eq!(
            display_path("/host/x\u{1b}]0;pwn\u{7}\ny"),
            "/host/x\\u{1b}]0;pwn\\u{7}\\u{a}y"
        );
        assert_eq!(
            host_for_terminal("satellite:box:2222:3").as_deref(),
            Some("box:2222")
        );
        assert_eq!(host_for_terminal("local:1"), None);
        assert_eq!(directory_for_terminal("file", "/host/a"), None);
        assert_eq!(
            directory_for_terminal("directory", "/host/a").as_deref(),
            Some("/host/a")
        );
    }
}
