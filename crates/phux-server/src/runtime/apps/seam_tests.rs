//! The host/app seam: only an app may reach its engine and its own internals.
//! The compiler already keeps VT and PTY crates out of the host's production
//! code; this keeps each engine's own API out too. A kind's needs belong on
//! [`super::ResourceApp`] or in its app, never in a host module.

use std::collections::BTreeMap;
use std::path::Path;

const TERMINAL_TOKENS: [&str; 12] = [
    "phux_terminal",
    "terminal_actor",
    "resource::terminal",
    "TerminalResource",
    ".terminal()",
    "TerminalHandle",
    "TerminalActor",
    "TerminalInput",
    "crate::agent_detect",
    "crate::native_state",
    "crate::grid",
    "crate::input",
];

/// The `AgentSession` engine's API and the app's own modules. `apps/mod.rs`
/// is where the app plugs in, so it alone may name the module.
const AGENT_SESSION_TOKENS: [&str; 6] = [
    "resource::agent_session",
    "AgentSessionActor",
    "AgentSessionResource",
    ".agent_session()",
    "apps::agent_session",
    "native_agents_mut()",
];

/// The persisted format names the type its records serialize as; the blob
/// is data, and renaming that type's path would not make it less so.
const AGENT_SESSION_EXEMPT: [&str; 2] = ["runtime/apps/mod.rs", "upgrade/blob.rs"];

fn is_test_source(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    path.split('/').any(|part| part == "tests")
        || name == "tests.rs"
        || name == "test_support.rs"
        || name.starts_with("tests_")
        || name.ends_with("_tests.rs")
}

fn count(source: &str, tokens: &[&str]) -> usize {
    source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .map(|line| {
            tokens
                .iter()
                .map(|token| line.matches(token).count())
                .sum::<usize>()
        })
        .sum()
}

fn walk(
    dir: &Path,
    root: &Path,
    tokens: &[&str],
    owns: fn(&str) -> bool,
    found: &mut BTreeMap<String, usize>,
) {
    for entry in std::fs::read_dir(dir).expect("read source dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            walk(&path, root, tokens, owns, found);
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .expect("under src")
            .to_string_lossy()
            .replace('\\', "/");
        let is_rust = path.extension().is_some_and(|ext| ext == "rs");
        if !is_rust || owns(&relative) || is_test_source(&relative) {
            continue;
        }
        let crossings = count(
            &std::fs::read_to_string(&path).expect("read source"),
            tokens,
        );
        if crossings > 0 {
            found.insert(relative, crossings);
        }
    }
}

/// Whether a line of source names the apps, in either import form.
fn names_the_apps(line: &str) -> bool {
    let code = line.split("//").next().unwrap_or(line);
    code.contains("runtime::apps") || (code.contains("runtime::{") && code.contains("apps"))
}

/// `state` sits below the runtime: an app may name it, never the reverse.
#[test]
fn state_never_reaches_up_into_the_apps() {
    fn scan(dir: &Path, root: &Path, hits: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).expect("read dir") {
            let path = entry.expect("entry").path();
            let relative = path.strip_prefix(root).expect("under src");
            if path.is_dir() {
                scan(&path, root, hits);
            } else if path.extension().is_some_and(|e| e == "rs")
                && !is_test_source(&relative.to_string_lossy())
                && std::fs::read_to_string(&path)
                    .expect("read source")
                    .lines()
                    .any(names_the_apps)
            {
                hits.push(relative.display().to_string());
            }
        }
    }
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("set by cargo and nextest");
    let src = Path::new(&manifest).join("src");
    let mut hits = Vec::new();
    scan(&src.join("state"), &src, &mut hits);
    assert!(hits.is_empty(), "state depends on the apps: {hits:?}");
}

#[test]
fn the_apps_check_reads_code_not_comments() {
    assert!(names_the_apps("use crate::runtime::apps::app_for;"));
    assert!(names_the_apps("use crate::runtime::{apps::app_for, pump};"));
    assert!(!names_the_apps("/// see runtime::apps for why"));
    assert!(!names_the_apps("let x = 1; // runtime::apps"));
}

fn crossings(tokens: &[&str], owns: fn(&str) -> bool) -> BTreeMap<String, usize> {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("set by the test runner");
    let src = Path::new(&manifest).join("src");
    let mut found = BTreeMap::new();
    walk(&src, &src, tokens, owns, &mut found);
    found
}

fn owns_terminal(path: &str) -> bool {
    path.starts_with("runtime/apps/terminal/")
}

fn owns_agent_session(path: &str) -> bool {
    path.starts_with("runtime/apps/agent_session/") || AGENT_SESSION_EXEMPT.contains(&path)
}

/// Frames about one resource reach its kind through `ResourceApp::frame`:
/// `connections/` names no stream helper and branches on no kind.
#[test]
fn connections_hand_resource_frames_to_the_app() {
    const HELPERS: [&str; 7] = [
        "is_agent_session",
        "handle_frame_ack",
        "handle_terminal_resize",
        "serve_history_request",
        "route_client_input",
        "dispatch_terminal_reply",
        "ResourceKind::AgentSession",
    ];
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("set by cargo and nextest");
    let src = Path::new(&manifest).join("src");
    let dir = src.join("runtime/connections");
    let hits: Vec<String> = std::fs::read_dir(&dir)
        .expect("read connections")
        .map(|entry| entry.expect("entry").path())
        .filter(|path| path.extension().is_some_and(|e| e == "rs"))
        .filter(|path| {
            let relative = path.strip_prefix(&src).expect("under src");
            !is_test_source(&relative.to_string_lossy())
        })
        .flat_map(|path| {
            let source = std::fs::read_to_string(&path).expect("read source");
            HELPERS
                .iter()
                .filter(|helper| source.contains(**helper))
                .map(|helper| format!("{}: {helper}", path.display()))
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(hits.is_empty(), "connections routes by kind: {hits:?}");
}

#[test]
fn the_host_reaches_the_terminal_engine_only_through_its_app() {
    let found = crossings(&TERMINAL_TOKENS, owns_terminal);
    assert!(
        found.is_empty(),
        "host modules name the Terminal engine; add to ResourceApp or move into runtime/apps/terminal: {found:?}"
    );
}

#[test]
fn the_host_reaches_the_agent_session_engine_only_through_its_app() {
    let found = crossings(&AGENT_SESSION_TOKENS, owns_agent_session);
    assert!(
        found.is_empty(),
        "host modules name the AgentSession engine or app; add to ResourceApp or move into runtime/apps/agent_session: {found:?}"
    );
}

#[test]
fn the_agent_session_check_reads_code_not_comments() {
    let tokens = &AGENT_SESSION_TOKENS;
    assert_eq!(count("use crate::resource::agent_session::Foo;", tokens), 1);
    assert_eq!(count("let s = handle.agent_session().ok();", tokens), 1);
    assert_eq!(
        count("crate::runtime::apps::agent_session::x();", tokens),
        1
    );
    assert_eq!(count("/// see resource::agent_session", tokens), 0);
    assert_eq!(
        count("phux_protocol::native_agent::NATIVE_SCHEMA", tokens),
        0
    );
}
