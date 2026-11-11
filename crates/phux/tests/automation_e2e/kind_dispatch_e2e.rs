//! `phux snapshot`, `phux wait`, `phux send-keys` and the other
//! Terminal-only verbs dispatch on their target's kind (phux-rbz0y.9,
//! phux-rbz0y.20), each run as its own `phux` subprocess
//! against a real server: a tree reads as `phux agent tree snapshot` reads
//! it, a document as its text, a surface only to `--out`, and a verb or flag
//! that does not apply to the kind exits 2.
//!
//! `#[ignore]`: they spawn a real server. Run via `just e2e`.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;

use std::io::Write as _;
use std::process::{Output, Stdio};

use common::ServerGuard;

/// A 1x1 PNG.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, 0x49, 0x48, 0x44, 0x52, 0, 0, 0,
    1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 0x1f, 0x15, 0xc4, 0x89, 0, 0, 0, 0x0d, 0x49, 0x44, 0x41, 0x54,
    0x78, 0x9c, 0x63, 0x60, 0, 0, 0, 2, 0, 1, 0xe5, 0x27, 0xde, 0xfc, 0, 0, 0, 0, 0x49, 0x45, 0x4e,
    0x44, 0xae, 0x42, 0x60, 0x82,
];

fn run(server: &ServerGuard, args: &[&str]) -> Output {
    server.cmd(args).output().expect("run phux")
}

/// `phux agent KIND open work ...`, then publish `bytes` to it; its `@N`.
fn opened(server: &ServerGuard, open: &[&str], kind: &str, bytes: &[u8]) -> String {
    let out = run(server, open);
    assert!(out.status.success(), "{open:?}: {out:?}");
    let id = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    assert!(id.starts_with('@'), "open prints the id: {id:?}");
    let mut publish = server
        .cmd(&["agent", kind, "publish", &id])
        .stdin(Stdio::piped())
        .spawn()
        .expect("publish");
    publish.stdin.take().unwrap().write_all(bytes).unwrap();
    assert!(publish.wait().unwrap().success(), "{kind} publish lands");
    id
}

fn error_code(out: &Output) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr);
    let line = stderr.lines().last().unwrap_or_default();
    let doc: serde_json::Value =
        serde_json::from_str(line).unwrap_or_else(|err| panic!("stderr {stderr:?}: {err}"));
    doc["error"]["code"].as_str().unwrap_or_default().to_owned()
}

/// Exit 2 with `code` under `--json`.
fn refused(server: &ServerGuard, args: &[&str], code: &str) {
    let out = run(server, args);
    assert_eq!(out.status.code(), Some(2), "{args:?}: {out:?}");
    assert!(out.stdout.is_empty(), "{args:?}: a refusal prints nothing");
    assert_eq!(error_code(&out), code, "{args:?}");
}

/// A server with one of each document kind, published once.
struct Fixture {
    server: ServerGuard,
    tree: String,
    notes: String,
    blob: String,
    surface: String,
}

impl Fixture {
    fn start() -> Self {
        let server = ServerGuard::builder("kinds")
            .seed_command("exec sleep 600")
            .start();
        let tree = opened(
            &server,
            &["agent", "tree", "open", "work", "--app", "smoke"],
            "tree",
            br#"{"title":"hello"}"#,
        );
        let notes = opened(
            &server,
            &["agent", "doc", "open", "work", "--type", "text/markdown"],
            "doc",
            b"# Plan\n## Done \x1b]52;c;aGk=\x07",
        );
        let binary = [
            "agent",
            "doc",
            "open",
            "work",
            "--type",
            "application/octet-stream",
        ];
        let blob = opened(&server, &binary, "doc", &[0, 1, 2]);
        let surface = opened(
            &server,
            &["agent", "surface", "open", "work", "--window", "smoke"],
            "surface",
            PNG,
        );
        Self {
            server,
            tree,
            notes,
            blob,
            surface,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        run(&self.server, args)
    }

    fn refused(&self, args: &[&str], code: &str) {
        refused(&self.server, args, code);
    }
}

#[test]
#[ignore = "spawns a real phux server; run via `just e2e`."]
fn snapshot_reads_each_document_kind_as_its_own_verb_does() {
    let f = Fixture::start();
    // A tree reads exactly as `phux agent tree snapshot` reads it.
    for json in [&[][..], &["--json"][..]] {
        let verb = f.run(&[&["snapshot"][..], json, &[f.tree.as_str()]].concat());
        let per_kind =
            f.run(&[&["agent", "tree", "snapshot"][..], json, &[f.tree.as_str()]].concat());
        assert!(verb.status.success(), "{verb:?}");
        assert_eq!(verb.stdout, per_kind.stdout, "{json:?}");
    }
    // A text document prints its content, safe for a terminal and ending
    // in a newline; a binary one its size and type.
    let shown = f.run(&["snapshot", &f.notes]).stdout;
    assert_eq!(
        String::from_utf8(shown).unwrap(),
        "# Plan\n## Done ␛]52;c;aGk=␇\n"
    );
    let per_kind = f.run(&["agent", "doc", "snapshot", &f.notes]).stdout;
    assert_eq!(
        String::from_utf8(per_kind).unwrap(),
        "# Plan\n## Done ␛]52;c;aGk=␇\n"
    );
    let binary = String::from_utf8(f.run(&["snapshot", &f.blob]).stdout).unwrap();
    assert!(
        binary.starts_with("3 bytes of application/octet-stream"),
        "{binary}"
    );
    let doc = f.run(&["snapshot", "--json", &f.notes]);
    let doc: serde_json::Value = serde_json::from_slice(&doc.stdout).unwrap();
    assert_eq!(
        doc["text"], "# Plan\n## Done \x1b]52;c;aGk=\x07",
        "--json is exact"
    );
    // A surface goes to a file only.
    f.refused(&["snapshot", "--json", &f.surface], "usage_error");
    let dir = tempfile::tempdir().unwrap();
    let frame = dir.path().join("frame.png");
    let frame_arg = frame.to_str().unwrap();
    let out = f.run(&["snapshot", "--out", frame_arg, &f.surface]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(std::fs::read(&frame).unwrap(), PNG);
    let saved = dir.path().join("notes.md");
    let out = f.run(&["snapshot", "--out", saved.to_str().unwrap(), &f.notes]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        std::fs::read(&saved).unwrap(),
        b"# Plan\n## Done \x1b]52;c;aGk=\x07",
        "--out is exact"
    );
    // Screen flags are a terminal's; `--out` is a document's.
    f.refused(
        &["snapshot", "--json", "--tail", "3", &f.tree],
        "usage_error",
    );
    f.refused(
        &["snapshot", "--json", "--out", frame_arg, "work"],
        "usage_error",
    );
    // A terminal is unchanged.
    let screen = f.run(&["snapshot", "--json", "work"]);
    let screen: serde_json::Value = serde_json::from_slice(&screen.stdout).unwrap();
    assert!(screen["cols"].as_u64().is_some(), "{screen}");
}

#[test]
#[ignore = "spawns a real phux server; run via `just e2e`."]
fn wait_follows_a_tree_or_a_text_document() {
    let f = Fixture::start();
    let wait = |args: &[&str]| f.run(&[&["wait", "--timeout", "5"][..], args].concat());
    assert_eq!(wait(&["--until", "hello", &f.tree]).status.code(), Some(0));
    assert_eq!(
        wait(&["--regex", "^## Done ", &f.notes]).status.code(),
        Some(0)
    );
    let missed = f.run(&["wait", "--timeout", "1", "--until", "never", &f.tree]);
    assert_eq!(missed.status.code(), Some(124), "{missed:?}");
    f.refused(
        &["wait", "--json", "--until", "x", &f.surface],
        "wrong_resource_kind",
    );
    f.refused(&["wait", "--json", "--until", "x", &f.blob], "usage_error");
    f.refused(&["wait", "--json", &f.tree], "usage_error");
    f.refused(
        &["wait", "--json", "--idle", "50", "--until", "x", &f.tree],
        "usage_error",
    );
}

#[test]
#[ignore = "spawns a real phux server; run via `just e2e`."]
fn send_keys_refuses_every_document_kind() {
    let f = Fixture::start();
    for id in [&f.tree, &f.notes, &f.surface] {
        f.refused(&["send-keys", "--json", id, "Enter"], "wrong_resource_kind");
    }
}

/// Every verb that reaches a Terminal only refuses a document or an agent
/// session up front, as `send-keys` does, instead of failing downstream
/// (`run`'s "no OSC-133 shell marks").
#[test]
#[ignore = "spawns a real phux server; run via `just e2e`."]
fn terminal_only_verbs_refuse_every_document_kind() {
    let f = Fixture::start();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("upload.txt");
    std::fs::write(&file, b"x").unwrap();
    let file = file.to_str().unwrap();
    let cast = dir.path().join("out.cast");
    let cast = cast.to_str().unwrap();
    let opened = f.run(&[
        "agent",
        "session",
        "open",
        "work",
        "--provider",
        "claude",
        "--json",
    ]);
    assert!(opened.status.success(), "{opened:?}");
    let opened: serde_json::Value = serde_json::from_slice(&opened.stdout).unwrap();
    let session = opened["resource"].as_str().unwrap().to_owned();
    let parent = opened["parent"].as_str().unwrap().to_owned();
    for id in [&f.tree, &f.notes, &f.surface, &session] {
        let id = id.as_str();
        // A dangerous signal is refused for the kind before it asks.
        f.refused(&["signal", "--json", id, "kill"], "wrong_resource_kind");
        for args in [
            &["run", "--json", "--timeout", "5", id, "true"][..],
            &["paste", "--json", id, "hi"],
            &["resize", "--json", id, "80x24"],
            &["upload", "--json", id, file],
            &["take", "--json", id],
            &["give", "--json", id],
            &["signal", "--json", id, "freeze"],
            &["ask", "--json", id, "ok?"],
            &["rec", "--json", "-o", cast, "--duration", "1", id],
            &["agent", "send-keys", "--json", id, "Enter"],
            &["agent", "prompt", "--json", id, "hi"],
            &[
                "agent", "answer", "--json", "--id", "q", "--choice", "1", id,
            ],
        ] {
            f.refused(args, "wrong_resource_kind");
        }
        let report = f.run(&["agent", "report-state", id, "working"]);
        assert_eq!(report.status.code(), Some(2), "{report:?}");
    }
    // An agent session's refusal names the pane it runs in.
    let signalled = f.run(&["signal", "--json", &session, "freeze"]);
    let stderr = String::from_utf8_lossy(&signalled.stderr);
    let doc: serde_json::Value =
        serde_json::from_str(stderr.lines().last().unwrap_or_default()).unwrap();
    let remedy = doc["remedy"].as_str().unwrap_or_default();
    assert!(
        remedy.contains(&format!("its parent pane {parent}")),
        "{remedy}"
    );
    // A Terminal still takes them.
    let pasted = f.run(&["paste", "--json", "work", "x"]);
    assert!(pasted.status.success(), "{pasted:?}");
}
