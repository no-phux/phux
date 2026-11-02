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

/// The `AgentSession` engine's API and the app's own modules.
const AGENT_SESSION_TOKENS: [&str; 6] = [
    "resource::agent_session",
    "AgentSessionActor",
    "AgentSessionResource",
    ".agent_session()",
    "apps::agent_session",
    "native_agents_mut()",
];

/// The one engine type the persisted format names, because its records
/// serialize as it. Allowed in `upgrade/blob.rs` and nowhere else.
const PERSISTED_AGENT_TYPE: &str = "crate::resource::agent_session::NativeAppendAccepted";

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

/// Each `use` statement that reaches the agent session through a brace, on
/// one line or several, which no literal path token sees.
fn braced_agent_session_imports(source: &str) -> usize {
    let code: Vec<&str> = source
        .lines()
        .map(|line| line.split("//").next().unwrap_or(line))
        .collect();
    code.join(" ")
        .split(';')
        .filter_map(|statement| {
            let statement = statement.trim_start();
            match statement.find(" use ") {
                _ if statement.starts_with("use ") => Some(statement),
                Some(at) => Some(&statement[at..]),
                None => None,
            }
        })
        .filter(|import| {
            import.contains("agent_session")
                && ["resource::{", "apps::{", "runtime::{"]
                    .iter()
                    .any(|opening| import.contains(opening))
        })
        .count()
}

fn terminal_crossings(path: &str, source: &str) -> usize {
    if path.starts_with("runtime/apps/terminal/") {
        return 0;
    }
    count(source, &TERMINAL_TOKENS)
}

fn agent_session_crossings(path: &str, source: &str) -> usize {
    if path.starts_with("runtime/apps/agent_session/") {
        return 0;
    }
    let source = if path == "upgrade/blob.rs" {
        source.replace(PERSISTED_AGENT_TYPE, "")
    } else {
        source.to_owned()
    };
    count(&source, &AGENT_SESSION_TOKENS) + braced_agent_session_imports(&source)
}

fn walk(
    dir: &Path,
    root: &Path,
    crossings_in: fn(&str, &str) -> usize,
    found: &mut BTreeMap<String, usize>,
) {
    for entry in std::fs::read_dir(dir).expect("read source dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            walk(&path, root, crossings_in, found);
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .expect("under src")
            .to_string_lossy()
            .replace('\\', "/");
        let is_rust = path.extension().is_some_and(|ext| ext == "rs");
        if !is_rust || is_test_source(&relative) {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("read source");
        let crossings = crossings_in(&relative, &source);
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

fn crossings(crossings_in: fn(&str, &str) -> usize) -> BTreeMap<String, usize> {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("set by the test runner");
    let src = Path::new(&manifest).join("src");
    let mut found = BTreeMap::new();
    walk(&src, &src, crossings_in, &mut found);
    found
}

#[test]
fn the_host_reaches_the_terminal_engine_only_through_its_app() {
    let found = crossings(terminal_crossings);
    assert!(
        found.is_empty(),
        "host modules name the Terminal engine; add to ResourceApp or move into runtime/apps/terminal: {found:?}"
    );
}

#[test]
fn the_host_reaches_the_agent_session_engine_only_through_its_app() {
    let found = crossings(agent_session_crossings);
    assert!(
        found.is_empty(),
        "host modules name the AgentSession engine or app; add to ResourceApp or move into runtime/apps/agent_session: {found:?}"
    );
}

#[test]
fn the_agent_session_check_sees_braced_imports() {
    let host = |source| agent_session_crossings("runtime/whoami.rs", source);
    assert_eq!(host("use crate::resource::{Foo, agent_session::Bar};"), 1);
    assert_eq!(host("use crate::runtime::apps::{agent_session};"), 1);
    assert_eq!(
        host("pub(crate) use crate::resource::{\n    Foo,\n    agent_session::Bar,\n};"),
        1
    );
    assert_eq!(host("use crate::resource::{Foo, ResourceHandle};"), 0);
    assert_eq!(host("// use crate::resource::{agent_session::Bar};"), 0);
}

#[test]
fn only_the_blob_may_name_only_its_persisted_type() {
    let field = "pub x: Vec<crate::resource::agent_session::NativeAppendAccepted>,";
    assert_eq!(agent_session_crossings("upgrade/blob.rs", field), 0);
    assert_eq!(agent_session_crossings("runtime/whoami.rs", field), 1);
    let other = "let s = handle.agent_session().ok();";
    assert_eq!(agent_session_crossings("upgrade/blob.rs", other), 1);
    let engine = "use crate::resource::agent_session::AgentSessionActor;";
    assert!(agent_session_crossings("upgrade/blob.rs", engine) > 0);
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
