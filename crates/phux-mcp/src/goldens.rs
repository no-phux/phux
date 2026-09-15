//! The two acceptance tests for moving tools off the CLI subprocess.
//!
//! - `in_process_tools_produce_the_same_documents_as_the_former_subprocess_path`
//!   drives every migrated tool against the shared scripted server and
//!   byte-compares its pretty-printed result with a checked-in golden under
//!   `tests/golden/`. The goldens are regression pins generated from the
//!   in-process code, not captures from the old path. Parity with the
//!   former subprocess path holds by construction: a CLI `--json` document
//!   is built by the same `phux-client` function the CLI prints
//!   (`session_list::document`, `snapshot::project`,
//!   `spawn::spawned_document`, `spatial::run`), and every MCP envelope
//!   (`kill`, `signal`, `tag`, `rename`, `detach`, `agent set/clear`) keeps
//!   the literal shape the adapter built around a CLI exit before. It was
//!   also checked once, side by side, against a real server with the
//!   origin/main adapter. The insert-pane and move-pane goldens pin the CLI
//!   contract: the former argv passed `--horizontal`/`--vertical`, which
//!   `phux` rejects, so that path never produced a document.
//! - `no_tool_outside_the_residue_spawns_a_subprocess` sweeps every catalog
//!   tool that `crate::tool_table` marks in-process (mirrors included) and
//!   proves, through the `CliAdapter` spawn record, that none reaches the
//!   CLI; a residue tool is the positive control.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]

use std::path::{Path, PathBuf};

use phux_client::agent_meta::{AgentMetaState, AgentRecord};
use phux_client::layout::{SplitDir, Workspace};
use phux_client::layout_ops::{
    DEFAULT_LAYOUT_GROUP_ID, LayoutMutation, apply_mutation, layout_key,
};
use phux_client::snapshot::{ScreenState, SoftWrap};
use phux_client::testkit::{self, ScriptSpec};
use phux_protocol::ids::{ResourceId, ResourceKind, SessionId, WindowId};
use phux_protocol::wire::frame::{
    RESOURCE_AGENT_KEY, RESOURCE_TAGS_KEY, ResourceLifecycle, Scope, SpawnResult,
};
use phux_protocol::wire::info::{
    ExitFacet, ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo,
};
use serde_json::{Value, json};
use tokio::net::UnixListener;

use crate::cli_adapter::spawn_record;
use crate::tool_table::{Exec, TOOLS};

const WORK: SessionId = SessionId::new(1);
const SHELL: WindowId = WindowId::new(10);

/// Session `work` (id 1) with one window holding `@1` (focused) and `@2`.
fn state() -> SessionSnapshot {
    state_with(&[1, 2])
}

fn state_with(panes: &[u32]) -> SessionSnapshot {
    SessionSnapshot::new(WORK, SHELL, ResourceId::local(1))
        .with_sessions(vec![
            SessionInfo::new(WORK, "work")
                .with_window_count(1)
                .with_active_window(Some(SHELL)),
        ])
        .with_windows(vec![
            WindowInfo::new(SHELL, WORK, "shell")
                .with_index(0)
                .with_active_resource(Some(ResourceId::local(1))),
        ])
        .with_resources(
            panes
                .iter()
                .map(|id| ResourceInfo::new(ResourceId::local(*id), SHELL, 80, 24))
                .collect(),
        )
}

/// [`state`] plus an agent session under `@1` and a retained, exited `@4`:
/// every resource shape `phux ls --json` spells.
fn ls_state() -> SessionSnapshot {
    state().with_resources(vec![
        ResourceInfo::new(ResourceId::local(1), SHELL, 80, 24),
        ResourceInfo::new(ResourceId::local(2), SHELL, 80, 24),
        ResourceInfo::new(ResourceId::local(9), SHELL, 0, 0)
            .with_kind(ResourceKind::AgentSession)
            .with_parent(Some(ResourceId::local(1))),
        ResourceInfo::new(ResourceId::local(4), SHELL, 80, 24)
            .with_lifecycle(ResourceLifecycle::Exited)
            .with_exit(Some(ExitFacet::new(5, 10).with_exit_status(Some(42)))),
    ])
}

fn screen() -> ScreenState {
    ScreenState {
        pane: 1,
        cols: 9,
        rows: 2,
        lines: vec!["the quick".to_owned(), "brown fox".to_owned()],
        scrollback: vec!["h1".to_owned(), "h2".to_owned(), "h3".to_owned()],
        soft_wrap: Some(SoftWrap {
            lines: vec![0],
            scrollback: Vec::new(),
        }),
        title: Some("zsh".to_owned()),
        ..ScreenState::default()
    }
}

/// Session `work`'s stored layout holding `panes`, split in order.
fn layout(panes: &[u32]) -> Vec<u8> {
    let mut workspace = Workspace::single(ResourceId::local(panes[0]));
    for pair in panes.windows(2) {
        apply_mutation(
            &mut workspace,
            &LayoutMutation::Split {
                target: ResourceId::local(pair[0]),
                new_pane: ResourceId::local(pair[1]),
                dir: SplitDir::Horizontal,
                ratio: 0.5,
            },
        )
        .expect("fixture layout");
    }
    workspace.encode_cbor().expect("fixture layout encodes")
}

fn with_layout(spec: ScriptSpec, panes: &[u32]) -> ScriptSpec {
    spec.stored_metadata(
        Scope::Group(DEFAULT_LAYOUT_GROUP_ID),
        &layout_key(WORK),
        layout(panes),
    )
}

fn tags(values: &[&str]) -> Vec<u8> {
    serde_json::to_vec(values).expect("tags encode")
}

/// One migrated tool's golden scenario.
struct Case {
    golden: &'static str,
    tool: &'static str,
    args: fn() -> Value,
    spec: fn() -> ScriptSpec,
}

const CASES: &[Case] = &[
    Case {
        golden: "ls",
        tool: "phux_ls",
        args: || json!({}),
        spec: || ScriptSpec::new().state(ls_state()),
    },
    Case {
        golden: "snapshot_tail_unwrap",
        tool: "phux_snapshot",
        args: || json!({ "target": "@1", "tail": 3, "unwrap": true }),
        spec: || ScriptSpec::new().state(state()).screen(&screen()),
    },
    Case {
        golden: "kill",
        tool: "phux_kill",
        args: || json!({ "target": "@2", "confirm": true }),
        spec: || ScriptSpec::new().state(state()),
    },
    Case {
        golden: "kill_session",
        tool: "phux_kill",
        args: || json!({ "target": "work", "confirm": true }),
        spec: || ScriptSpec::new().state(state()),
    },
    Case {
        golden: "detach",
        tool: "phux_detach",
        args: || json!({ "session": "work", "confirm": true }),
        spec: || ScriptSpec::new().detach_result(2),
    },
    Case {
        golden: "spawn",
        tool: "phux_spawn",
        args: || json!({ "cwd": "/repo", "command": ["cargo", "test"] }),
        spec: || ScriptSpec::new().spawn_result(SpawnResult::Ok(ResourceId::local(7))),
    },
    Case {
        golden: "spawn_placed",
        tool: "phux_spawn",
        args: || json!({ "target": "@1", "split": "vertical", "ratio": 0.3 }),
        spec: || {
            ScriptSpec::new()
                .state(state_with(&[1, 2, 7]))
                .spawn_result(SpawnResult::Ok(ResourceId::local(7)))
        },
    },
    Case {
        golden: "signal",
        tool: "phux_signal",
        args: || json!({ "target": "@1", "signal": "freeze" }),
        spec: || ScriptSpec::new().state(state()),
    },
    Case {
        golden: "tag_ls",
        tool: "phux_tag",
        args: || json!({ "action": "ls", "target": "work" }),
        spec: || {
            ScriptSpec::new().state(state()).stored_metadata(
                Scope::Resource(ResourceId::local(1)),
                RESOURCE_TAGS_KEY,
                tags(&["build", "ci"]),
            )
        },
    },
    Case {
        golden: "tag_add",
        tool: "phux_tag",
        args: || json!({ "action": "add", "target": "@2", "tags": ["#urgent", "build"] }),
        spec: || {
            ScriptSpec::new().state(state()).stored_metadata(
                Scope::Resource(ResourceId::local(2)),
                RESOURCE_TAGS_KEY,
                tags(&["build"]),
            )
        },
    },
    Case {
        golden: "rename",
        tool: "phux_rename",
        args: || json!({ "session": "work", "new_name": "play" }),
        spec: || ScriptSpec::new().state(state()),
    },
    Case {
        golden: "insert_pane",
        tool: "phux_insert_pane",
        args: || json!({ "target": "@1", "new_pane": "@2", "direction": "vertical", "ratio": 0.3 }),
        spec: || with_layout(ScriptSpec::new().state(state()), &[1]),
    },
    Case {
        golden: "move_pane",
        tool: "phux_move_pane",
        args: || json!({ "source": "@2", "target": "@1" }),
        spec: || with_layout(ScriptSpec::new().state(state()), &[1, 2]),
    },
    Case {
        golden: "swap_pane",
        tool: "phux_swap_pane",
        args: || json!({ "first": "@1", "second": "@2" }),
        spec: || with_layout(ScriptSpec::new().state(state()), &[1, 2]),
    },
    Case {
        golden: "agent_set",
        tool: "phux_agent_set",
        args: || json!({ "target": "@1", "name": "bot", "kind": "codex", "state": "working" }),
        spec: || ScriptSpec::new().state(state()),
    },
    Case {
        golden: "agent_clear",
        tool: "phux_agent_clear",
        args: || json!({ "target": "@1" }),
        spec: || {
            let record = AgentRecord {
                name: "bot".to_owned(),
                state: AgentMetaState::Working,
                ..AgentRecord::default()
            };
            ScriptSpec::new().state(state()).stored_metadata(
                Scope::Resource(ResourceId::local(1)),
                RESOURCE_AGENT_KEY,
                record.encode(),
            )
        },
    },
];

fn golden_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.json"))
}

/// Run `case` against a scripted server that answers every connection the
/// tool dials with a fresh copy of its spec, and return the pretty-printed
/// document exactly as `tools/call` would carry it.
async fn run(case: &Case) -> String {
    let dir = tempfile::tempdir().expect("temp dir");
    let socket = dir.path().join("golden.sock");
    let listener = UnixListener::bind(&socket).expect("bind");
    let server = tokio::spawn(testkit::serve_every(listener, case.spec));
    let mut args = (case.args)();
    args["socket"] = json!(socket.to_string_lossy());
    let result = crate::tools::dispatch(case.tool, &args).await;
    server.abort();
    let value = result.unwrap_or_else(|err| panic!("{} failed: {}", case.golden, err.0));
    serde_json::to_string_pretty(&value).expect("pretty") + "\n"
}

#[tokio::test]
async fn in_process_tools_produce_the_same_documents_as_the_former_subprocess_path() {
    let mut mismatches = Vec::new();
    for case in CASES {
        let actual = run(case).await;
        let path = golden_path(case.golden);
        match std::fs::read_to_string(&path) {
            Ok(expected) if expected == actual => {}
            Ok(expected) => mismatches.push(format!(
                "{}: document drifted\n--- golden\n{expected}--- actual\n{actual}",
                case.golden
            )),
            Err(_) => mismatches.push(format!(
                "{}: no golden at {}; the document is\n{actual}",
                case.golden,
                path.display()
            )),
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    assert!(
        spawn_record::take().is_empty(),
        "a migrated tool spawned the CLI"
    );
}

/// Every in-process tool with minimal valid arguments against a socket
/// nothing listens on: each fails at connect (or earlier), and none may
/// reach the CLI adapter.
fn sweep_args(tool: &str) -> Value {
    let dead = "/nonexistent/phux-mcp-sweep.sock";
    let mut args = match tool {
        "phux_ls" | "phux_spawn" | "phux_agent_clear" => json!({}),
        "phux_snapshot" | "phux_resource_methods" | "phux_resource_show" => {
            json!({ "target": "@1" })
        }
        "phux_wait" | "phux_watch" | "phux_resource_wait" => {
            json!({ "target": "@1", "timeout_secs": 1 })
        }
        "phux_send_keys" => json!({ "target": "@1", "keys": ["x"] }),
        "phux_paste" => json!({ "target": "@1", "text": "x" }),
        "phux_kill" => json!({ "target": "@1", "confirm": true }),
        "phux_detach" => json!({ "confirm": true }),
        "phux_ask" => json!({ "target": "@1", "id": "q", "question": "ok?" }),
        "phux_signal" => json!({ "target": "@1", "signal": "freeze" }),
        "phux_tag" => json!({ "action": "ls", "target": "@1" }),
        "phux_rename" => json!({ "session": "a", "new_name": "b" }),
        "phux_insert_pane" => json!({ "target": "@1", "new_pane": "@2" }),
        "phux_move_pane" => json!({ "source": "@1", "target": "@2" }),
        "phux_swap_pane" => json!({ "first": "@1", "second": "@2" }),
        "phux_agent_set" => json!({ "name": "bot" }),
        "phux_plugin_action" | "phux_plugin_workspace" => {
            return json!({
                "plugin_id": "none", "action_id": "none",
                "config": "/nonexistent/phux-mcp-sweep/config.toml",
            });
        }
        other => panic!("{other} is in-process but has no sweep arguments"),
    };
    args["socket"] = json!(dead);
    args
}

#[tokio::test]
async fn no_tool_outside_the_residue_spawns_a_subprocess() {
    let catalog = crate::tools::catalog();
    let names: Vec<&str> = catalog
        .as_array()
        .expect("catalog")
        .iter()
        .map(|tool| tool["name"].as_str().expect("name"))
        .collect();
    let _ = spawn_record::take();
    for row in TOOLS.iter().filter(|row| !matches!(row.exec, Exec::Cli(_))) {
        assert!(names.contains(&row.name), "stale row {}", row.name);
        let result = crate::tools::dispatch(row.name, &sweep_args(row.name)).await;
        if let Err(err) = &result {
            assert!(
                !err.0.contains("must not spawn the phux CLI"),
                "{} reached the CLI adapter: {}",
                row.name,
                err.0
            );
        }
        assert_eq!(
            spawn_record::take(),
            Vec::<String>::new(),
            "{} spawned the CLI",
            row.name
        );
    }

    // Positive control: a residue tool does reach the adapter, so an empty
    // record above is evidence, not a recorder that never fires.
    let _ = crate::tools::dispatch(
        "phux_doctor",
        &json!({ "socket": "/nonexistent/phux-mcp-sweep.sock" }),
    )
    .await;
    assert_eq!(spawn_record::take().len(), 1, "the residue control spawned");
}
