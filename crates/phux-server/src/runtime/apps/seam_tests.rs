//! The host/app seam: only `runtime/apps/terminal` may reach the Terminal
//! engine. The compiler already keeps VT and PTY crates out of the host's
//! production code; this keeps the engine's own API out too. A kind's needs
//! belong on [`super::ResourceApp`] or in its app, never in a host module.

use std::collections::BTreeMap;
use std::path::Path;

const TOKENS: [&str; 12] = [
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

fn is_test_source(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    path.split('/').any(|part| part == "tests")
        || name == "tests.rs"
        || name == "test_support.rs"
        || name.starts_with("tests_")
        || name.ends_with("_tests.rs")
}

fn count(source: &str) -> usize {
    source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .map(|line| {
            TOKENS
                .iter()
                .map(|token| line.matches(token).count())
                .sum::<usize>()
        })
        .sum()
}

fn walk(dir: &Path, root: &Path, found: &mut BTreeMap<String, usize>) {
    for entry in std::fs::read_dir(dir).expect("read source dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            walk(&path, root, found);
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .expect("under src")
            .to_string_lossy()
            .replace('\\', "/");
        let is_rust = path.extension().is_some_and(|ext| ext == "rs");
        if !is_rust || relative.starts_with("runtime/apps/terminal/") || is_test_source(&relative) {
            continue;
        }
        let crossings = count(&std::fs::read_to_string(&path).expect("read source"));
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

/// Names that would route an input or stream frame by kind outside the apps.
const FRAME_ROUTING_TOKENS: [&str; 6] = [
    "is_input_frame",
    "is_stream_frame",
    "handle_frame_ack",
    "serve_history_request",
    "handle_terminal_resize",
    "is_agent_session",
];

/// Connection dispatch hands an input or stream frame to the app serving
/// the resource it addresses ([`super::ResourceApp::stream_frame`]); it
/// never picks a kind's handler or refusal itself.
#[test]
fn connections_route_stream_frames_only_through_the_apps() {
    fn scan(dir: &Path, root: &Path, hits: &mut BTreeMap<String, Vec<&'static str>>) {
        for entry in std::fs::read_dir(dir).expect("read dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                scan(&path, root, hits);
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .expect("under src")
                .to_string_lossy()
                .replace('\\', "/");
            if path.extension().is_none_or(|ext| ext != "rs") || is_test_source(&relative) {
                continue;
            }
            let source = std::fs::read_to_string(&path).expect("read source");
            let named: Vec<&'static str> = FRAME_ROUTING_TOKENS
                .into_iter()
                .filter(|token| {
                    source
                        .lines()
                        .any(|line| line.split("//").next().unwrap_or(line).contains(token))
                })
                .collect();
            if !named.is_empty() {
                hits.insert(relative, named);
            }
        }
    }
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("set by the test runner");
    let src = Path::new(&manifest).join("src");
    let mut hits = BTreeMap::new();
    scan(&src.join("runtime/connections"), &src, &mut hits);
    assert!(
        hits.is_empty(),
        "connections route stream frames by kind; add to ResourceApp::stream_frame instead: {hits:?}"
    );
}

#[test]
fn the_host_reaches_the_terminal_engine_only_through_its_app() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("set by the test runner");
    let src = Path::new(&manifest).join("src");
    let mut found = BTreeMap::new();
    walk(&src, &src, &mut found);
    assert!(
        found.is_empty(),
        "host modules name the Terminal engine; add to ResourceApp or move into runtime/apps/terminal: {found:?}"
    );
}
