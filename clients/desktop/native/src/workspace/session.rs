//! Arguments for `phux` session commands on one explicit socket.
//!
//! These do not invent a host or inherit the desktop process directory.

/// Arguments for `phux new` that create a session on one socket in one directory.
#[must_use]
pub fn create_session_args(socket: &str, name: &str, directory: &str) -> Vec<String> {
    vec![
        "--socket".to_owned(),
        socket.to_owned(),
        "new".to_owned(),
        "--json".to_owned(),
        "-s".to_owned(),
        name.to_owned(),
        "--cwd".to_owned(),
        directory.to_owned(),
    ]
}

/// Arguments for `phux rename` on one socket.
#[must_use]
pub fn rename_session_args(socket: &str, current: &str, next: &str) -> Vec<String> {
    vec![
        "--socket".to_owned(),
        socket.to_owned(),
        "rename".to_owned(),
        current.to_owned(),
        next.to_owned(),
    ]
}

/// Arguments for `phux kill` of one session on one socket.
#[must_use]
pub fn close_session_args(socket: &str, name: &str) -> Vec<String> {
    vec![
        "--socket".to_owned(),
        socket.to_owned(),
        "kill".to_owned(),
        name.to_owned(),
    ]
}

/// The session a new window attaches. An empty pick keeps the current window.
#[must_use]
pub fn window_session(explicit: Option<&str>, current: &str) -> String {
    let name = explicit.unwrap_or("").trim();
    if name.is_empty() {
        current.to_owned()
    } else {
        name.to_owned()
    }
}

/// Refuse a create that would fall back to the desktop process directory.
#[must_use]
pub fn create_session_error(name: &str, directory: &str) -> Option<&'static str> {
    if name.trim().is_empty() {
        return Some("A session needs a name.");
    }
    if directory.trim().is_empty() {
        return Some("A new session needs the focused directory or a chosen folder.");
    }
    None
}

/// Refuse a rename that names neither side. An unchanged name is not an error.
#[must_use]
pub fn rename_session_error(current: &str, next: &str) -> Option<&'static str> {
    if current.trim().is_empty() || next.trim().is_empty() {
        return Some("Rename needs the current session and a new name.");
    }
    None
}
