//! A ratchet on the kind references the host still makes. Each scan lists
//! the files that match today, how often, and the issue that removes them.
//! It fails when a new file matches or a listed file's count changes, so the
//! list only shrinks: fix the code, or lower the baseline to what it prints.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::seam_tests::{count, walk};

/// One baseline row: file, matches, and why it still matches.
type Row = (&'static str, usize, &'static str);

struct Scan {
    name: &'static str,
    /// The crate whose `src/` is scanned.
    krate: &'static str,
    /// Matches in one non-test file, by its path under `src/`.
    matches: fn(&str, &str) -> usize,
    baseline: &'static [Row],
}

/// The app a path under `runtime/apps/<app>/` belongs to.
fn app_of(path: &str) -> Option<&str> {
    path.strip_prefix("runtime/apps/")?
        .split_once('/')
        .map(|(app, _)| app)
}

/// `source` without its inline `#[cfg(test)] mod … { … }` blocks. A block
/// ends where its braces balance, so code after it is still scanned.
fn without_inline_tests(source: &str) -> String {
    let mut kept = String::with_capacity(source.len());
    let mut pending = String::new();
    let mut gated = false;
    let mut depth = 0usize;
    for line in source.split_inclusive('\n') {
        if depth > 0 {
            depth = balance(depth, line);
            continue;
        }
        let code = line.trim();
        if code == "#[cfg(test)]" || (gated && code.starts_with("#[")) {
            gated = true;
            pending.push_str(line);
            continue;
        }
        if gated && code.starts_with("mod ") && code.ends_with('{') {
            depth = balance(0, line);
            pending.clear();
            gated = false;
            continue;
        }
        kept.push_str(&pending);
        pending.clear();
        gated = false;
        kept.push_str(line);
    }
    kept.push_str(&pending);
    kept
}

/// Brace depth after `line`, starting from `depth`.
fn balance(depth: usize, line: &str) -> usize {
    line.chars().fold(depth, |depth, c| match c {
        '{' => depth + 1,
        '}' => depth.saturating_sub(1),
        _ => depth,
    })
}

fn tokens_outside(source: &str, tokens: &[&str]) -> usize {
    count(&without_inline_tests(source), tokens)
}

/// The document engine's API, outside the document app.
fn document_engine(path: &str, source: &str) -> usize {
    const TOKENS: [&str; 4] = [
        "phux_tree",
        "DocumentActor",
        ".document::<",
        "apps::document",
    ];
    if app_of(path) == Some("document") {
        return 0;
    }
    tokens_outside(source, &TOKENS)
}

/// The Terminal app's modules, named from outside it.
fn terminal_app_paths(path: &str, source: &str) -> usize {
    if app_of(path) == Some("terminal") {
        return 0;
    }
    tokens_outside(source, &["apps::terminal::", "super::terminal::"])
}

/// An app routing through the host's app table instead of serving its kind.
fn app_table_in_an_app(path: &str, source: &str) -> usize {
    const TOKENS: [&str; 4] = ["app_for(", "app_owning(", "app_serving(", "apps::APPS"];
    if app_of(path).is_none() {
        return 0;
    }
    tokens_outside(source, &TOKENS)
}

/// A host branch on a kind. `connections/` has its own zero-tolerance check.
fn host_kind_branches(path: &str, source: &str) -> usize {
    if app_of(path).is_some() || path.starts_with("runtime/connections/") {
        return 0;
    }
    tokens_outside(source, &["ResourceKind::"])
}

/// A `state/` module named for one kind's internals, which belong in its app.
fn app_internals_in_state(path: &str, _source: &str) -> usize {
    const NAMES: [&str; 6] = ["terminal", "agent", "document", "tree", "surface", "mirror"];
    let Some(rest) = path.strip_prefix("state/") else {
        return 0;
    };
    usize::from(NAMES.iter().any(|name| rest.contains(name)))
}

/// A kind branch in the organization model, outside the kind's definition.
fn core_kind_branches(path: &str, source: &str) -> usize {
    if path == "resource.rs" {
        return 0;
    }
    tokens_outside(source, &["ResourceKind::"])
}

const INPUT_LANE: &str = "the connection's Terminal input lane (phux-rbz0y.14)";
const STREAMS: &str = "Terminal streams and detector on the connection (phux-rbz0y.14)";
const STANDING: &str = "standing mirrors run in Terminal panes (phux-rbz0y.14)";
const LAUNCH: &str = "a Terminal launch command (phux-rbz0y.7)";
const CODEC: &str = "the Terminal codec gate at a host boundary (phux-rbz0y.7)";
const CONFORMANCE: &str = "test-only conformance fixture drives a Terminal";
const KIND_SPEC: &str = "per-kind rule not yet on KindSpec (phux-rbz0y.7)";
const SLOTS: &str = "per-app ServerState slot (phux-rbz0y.14)";

const SCANS: [Scan; 6] = [
    Scan {
        name: "document engine outside its app",
        krate: "phux-server",
        matches: document_engine,
        baseline: &[(
            "runtime/dispatch_guard.rs",
            1,
            "document input audit from the host guard (phux-rbz0y.14)",
        )],
    },
    Scan {
        name: "Terminal app modules named from outside it",
        krate: "phux-server",
        matches: terminal_app_paths,
        baseline: &[
            ("connector.rs", 1, INPUT_LANE),
            ("runtime/actor_conformance.rs", 1, CONFORMANCE),
            (
                "runtime/apps/agent_session/native_agent/launch.rs",
                5,
                "the native agent host runs in a Terminal (phux-rbz0y.7)",
            ),
            ("runtime/apps/spawn.rs", 2, CODEC),
            (
                "runtime/commands.rs",
                5,
                "Terminal commands re-exported to the host (phux-rbz0y.14)",
            ),
            ("runtime/commands/dispatch.rs", 2, STANDING),
            ("runtime/commands/lifecycle.rs", 1, LAUNCH),
            ("runtime/commands/satellite_lease.rs", 1, CODEC),
            ("runtime/config.rs", 1, LAUNCH),
            ("runtime/connections/commands.rs", 2, INPUT_LANE),
            ("runtime/connections/dispatch.rs", 2, STREAMS),
            ("runtime/connections/frames.rs", 3, STREAMS),
            ("runtime/connections/lifecycle.rs", 1, INPUT_LANE),
            ("runtime/connections/metadata_mutation.rs", 2, STANDING),
            ("runtime/connections/mod.rs", 2, STREAMS),
            ("runtime/connections/pane_exit.rs", 1, STREAMS),
            ("runtime/connections/plumbing.rs", 1, STREAMS),
            ("runtime/connections/resource_frames.rs", 2, STREAMS),
            ("runtime/delegation.rs", 1, INPUT_LANE),
            ("runtime/delegation/sockets.rs", 1, INPUT_LANE),
            (
                "runtime/download.rs",
                1,
                "file digests live in the Terminal upload module (phux-rbz0y.14)",
            ),
            ("runtime/ephemeral_listener.rs", 1, INPUT_LANE),
            ("runtime/listeners/mod.rs", 1, INPUT_LANE),
            ("runtime/mod.rs", 1, STANDING),
            ("runtime/revocation_conformance.rs", 2, CONFORMANCE),
            (
                "runtime/revocation_conformance/in_flight.rs",
                1,
                CONFORMANCE,
            ),
            ("runtime/scope_matrix.rs", 1, INPUT_LANE),
            ("runtime/scope_matrix/guard.rs", 1, INPUT_LANE),
            ("runtime/server/mod.rs", 2, STANDING),
            ("runtime/server/serve.rs", 2, STANDING),
            ("runtime/services.rs", 1, INPUT_LANE),
            (
                "runtime/session_attach/mod.rs",
                2,
                "session attach stages Terminal panes itself (phux-rbz0y.14)",
            ),
            ("runtime/session_attach/prepare.rs", 1, CODEC),
            ("runtime/startup.rs", 2, LAUNCH),
        ],
    },
    Scan {
        name: "an app routing through the host's app table",
        krate: "phux-server",
        matches: app_table_in_an_app,
        baseline: &[],
    },
    Scan {
        name: "host branches on a kind",
        krate: "phux-server",
        matches: host_kind_branches,
        baseline: &[
            ("hub/relay.rs", 2, KIND_SPEC),
            ("hub/relay/requests.rs", 1, KIND_SPEC),
            (
                "id_bridge.rs",
                20,
                "permanent: the one wire/core kind bridge",
            ),
            ("org_scale.rs", 1, LAUNCH),
            ("org_wire.rs", 5, KIND_SPEC),
            (
                "runtime/apps/mod.rs",
                10,
                "permanent: app_for, the one kind-to-app match",
            ),
            ("runtime/commands/satellite_lease.rs", 1, KIND_SPEC),
            ("runtime/organization/translate.rs", 3, LAUNCH),
            ("state/bindings.rs", 1, SLOTS),
            ("state/legacy_snapshot.rs", 4, KIND_SPEC),
            ("state/native_agent.rs", 1, SLOTS),
            (
                "state/org/create.rs",
                2,
                "kind-typed organization creates (phux-rbz0y.14)",
            ),
            ("state/org/creation_bounds.rs", 1, LAUNCH),
            ("state/session_table.rs", 2, KIND_SPEC),
            (
                "state/upgrade_blob/import.rs",
                2,
                "permanent: a pre-record blob's frozen panes and agent sessions import as those kinds",
            ),
        ],
    },
    Scan {
        name: "app internals in state/",
        krate: "phux-server",
        matches: app_internals_in_state,
        baseline: &[
            ("state/agent.rs", 1, SLOTS),
            ("state/agent_tracking.rs", 1, SLOTS),
            ("state/mirror_demand.rs", 1, SLOTS),
            ("state/native_agent.rs", 1, SLOTS),
            ("state/native_agent/validate.rs", 1, SLOTS),
            ("state/terminals.rs", 1, SLOTS),
        ],
    },
    Scan {
        name: "phux-core branches on a kind",
        krate: "phux-core",
        matches: core_kind_branches,
        baseline: &[
            (
                "apply/validate.rs",
                1,
                "a Terminal needs a launch spec (phux-rbz0y.7)",
            ),
            ("registry.rs", 6, "facets seeded per kind (phux-rbz0y.7)"),
        ],
    },
];

fn src_of(krate: &str) -> PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("set by cargo and nextest");
    Path::new(&manifest).join("..").join(krate).join("src")
}

/// What `scan` finds that its baseline does not say, one line per file.
fn drift(scan: &Scan, found: &BTreeMap<String, usize>) -> Vec<String> {
    let baseline: BTreeMap<&str, usize> = scan
        .baseline
        .iter()
        .map(|(path, matches, _)| (*path, *matches))
        .collect();
    let mut lines: Vec<String> = found
        .iter()
        .filter(|(path, matches)| baseline.get(path.as_str()) != Some(*matches))
        .map(|(path, matches)| {
            let was = baseline.get(path.as_str()).copied().unwrap_or(0);
            format!("{path}: {was} -> {matches}")
        })
        .collect();
    lines.extend(
        baseline
            .iter()
            .filter(|(path, _)| !found.contains_key(**path))
            .map(|(path, was)| format!("{path}: {was} -> 0, drop its row")),
    );
    lines
}

#[test]
fn host_kind_references_only_shrink() {
    let failures: Vec<String> = SCANS
        .iter()
        .filter_map(|scan| {
            let src = src_of(scan.krate);
            let mut found = BTreeMap::new();
            walk(&src, &src, scan.matches, &mut found);
            let lines = drift(scan, &found);
            (!lines.is_empty()).then(|| format!("{}:\n  {}", scan.name, lines.join("\n  ")))
        })
        .collect();
    assert!(
        failures.is_empty(),
        "a kind reference moved; put a new one behind ResourceApp or its app, \
         or lower the baseline in seam_ratchet_tests.rs:\n{}",
        failures.join("\n")
    );
}

#[test]
fn every_baseline_row_names_its_reason_once() {
    for scan in &SCANS {
        let mut seen = std::collections::BTreeSet::new();
        for (path, matches, reason) in scan.baseline {
            assert!(*matches > 0 && !reason.is_empty(), "{}: {path}", scan.name);
            assert!(seen.insert(*path), "{}: {path} listed twice", scan.name);
        }
    }
}

#[test]
fn the_scans_skip_inline_tests_and_their_own_app() {
    let source =
        "use a::ResourceKind::X;\n#[cfg(test)]\n#[allow(x)]\nmod tests {\n    ResourceKind::Y\n}\n";
    assert_eq!(host_kind_branches("state/x.rs", source), 1);
    let gated_import = "#[cfg(test)]\nuse b;\nResourceKind::Z\n";
    assert_eq!(host_kind_branches("state/x.rs", gated_import), 1);
    assert_eq!(host_kind_branches("runtime/apps/document/x.rs", source), 0);
    assert_eq!(document_engine("state/x.rs", "use phux_tree::X;"), 1);
    assert_eq!(
        document_engine("runtime/apps/document/x.rs", "use phux_tree::X;"),
        0
    );
    assert_eq!(
        app_table_in_an_app("runtime/apps/terminal/x.rs", "app_for(k)"),
        1
    );
    assert_eq!(
        app_table_in_an_app("runtime/apps/spawn.rs", "app_for(k)"),
        0
    );
    assert_eq!(
        terminal_app_paths("runtime/x.rs", "// apps::terminal::y"),
        0
    );
    assert_eq!(app_internals_in_state("state/documents.rs", ""), 1);
    assert_eq!(app_internals_in_state("state/leases.rs", ""), 0);
}
