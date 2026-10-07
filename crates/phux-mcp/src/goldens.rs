//! Golden documents for the in-process tools, and the sweep proving no
//! in-process tool spawns the CLI.
//!
//! Each case drives one tool against the shared scripted server and
//! byte-compares its pretty-printed result with `tests/golden/<name>.json`.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]

use std::path::PathBuf;

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
        // The pre-check and the barrier are two `GET_STATE`s on one
        // connection. The scripted server stores `SET_METADATA` without
        // rewriting its snapshot, so the barrier answer is the applied name.
        spec: || {
            let mut applied = state();
            applied
                .sessions
                .first_mut()
                .expect("state has a session")
                .name = "play".to_owned();
            ScriptSpec::new().states([state(), applied])
        },
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
    std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("the test runner sets CARGO_MANIFEST_DIR"),
    )
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

/// A kill that hits under a partial fleet view still succeeds (the CLI
/// warns and continues). The shared builder is what prints that warning.
#[tokio::test]
async fn kill_under_a_partial_view_still_kills() {
    const NOTICE: &str = "satellite build-box is unreachable: link is down";
    let dir = tempfile::tempdir().expect("temp dir");
    let socket = dir.path().join("golden.sock");
    let listener = UnixListener::bind(&socket).expect("bind");
    let spec = || ScriptSpec::new().state(state()).degradation_notice(NOTICE);
    let server = tokio::spawn(testkit::serve_every(listener, spec));
    let result = crate::tools::dispatch(
        "phux_kill",
        &json!({
            "target": "@2",
            "confirm": true,
            "socket": socket.to_string_lossy(),
        }),
    )
    .await;
    server.abort();
    let value = result.expect("partial-view kill");
    assert_eq!(value["killed"], json!(true));
    assert_eq!(value["target"], "@2");
}

/// Every in-process tool with minimal valid arguments against a socket
/// nothing listens on: each fails at connect (or earlier), and none may
/// reach the CLI adapter.
fn sweep_args(tool: &str) -> Value {
    let dead = "/nonexistent/phux-mcp-sweep.sock";
    let mut args = match tool {
        "phux_ls" | "phux_spawn" | "phux_agent_clear" | "phux_approvals" => json!({}),
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
        "phux_approve" => json!({ "id": "0123456789abcdef0123456789abcdef", "decision": "deny" }),
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

/// [`state`] and [`screen`], with `@1` carrying `record` as its
/// `phux.agent/v1` record.
fn agent_spec(record: &AgentRecord) -> ScriptSpec {
    ScriptSpec::new()
        .state(state())
        .screen(&screen())
        .stored_metadata(
            Scope::Resource(ResourceId::local(1)),
            RESOURCE_AGENT_KEY,
            record.encode(),
        )
}

/// Dispatch `tool` with `args` against a scripted server answering every
/// connection with `spec()`.
async fn dispatch_against(
    tool: &str,
    mut args: Value,
    spec: impl Fn() -> ScriptSpec + Send + 'static,
) -> Result<Value, crate::tools::ToolError> {
    let dir = tempfile::tempdir().expect("temp dir");
    let socket = dir.path().join("agent.sock");
    let listener = UnixListener::bind(&socket).expect("bind");
    let server = tokio::spawn(testkit::serve_every(listener, spec));
    args["socket"] = json!(socket.to_string_lossy());
    let result = crate::tools::dispatch(tool, &args).await;
    server.abort();
    result
}

/// The `error.code` of a refusal on the `--json` error contract.
fn contract_code(err: &crate::tools::ToolError) -> String {
    let document: Value = serde_json::from_str(&err.0)
        .unwrap_or_else(|_| panic!("not a contract error document: {}", err.0));
    document["error"]["code"]
        .as_str()
        .expect("error.code")
        .to_owned()
}

/// ADR-0075 point 5 on MCP: `phux_send_keys` and `phux_paste` deliver input,
/// so a `%name` whose record has the withdrawn shape (a `kind`, `state:
/// unknown`) is refused as `agent_withdrawn`, exactly as the CLI input verbs
/// refuse it. Read tools still resolve it, and an identity-only record (no
/// `kind`) is the resting value of a declaration, not a withdrawal.
#[tokio::test]
async fn input_tools_refuse_a_withdrawn_name_that_read_tools_still_resolve() {
    let withdrawn = || {
        agent_spec(&AgentRecord {
            name: "build".to_owned(),
            kind: Some("codex".to_owned()),
            state: AgentMetaState::Unknown,
            ..AgentRecord::default()
        })
    };
    for (tool, args) in [
        (
            "phux_send_keys",
            json!({ "target": "%build", "keys": ["rm -rf .", "Enter"] }),
        ),
        (
            "phux_paste",
            json!({ "target": "%build", "text": "rm -rf ." }),
        ),
    ] {
        let err = dispatch_against(tool, args, withdrawn)
            .await
            .expect_err("a withdrawn name must not receive input");
        assert_eq!(contract_code(&err), "agent_withdrawn", "{tool}: {}", err.0);
    }

    let read = dispatch_against("phux_snapshot", json!({ "target": "%build" }), withdrawn)
        .await
        .expect("a read tool skips the write guard");
    assert_eq!(read["lines"][0], "the quick");
    let waited = dispatch_against(
        "phux_wait",
        json!({ "target": "%build", "until": "brown fox", "timeout_secs": 5 }),
        withdrawn,
    )
    .await
    .expect("a read tool skips the write guard");
    assert_eq!(waited["outcome"], "met");

    let identity_only = || {
        agent_spec(&AgentRecord {
            name: "build".to_owned(),
            kind: None,
            state: AgentMetaState::Unknown,
            ..AgentRecord::default()
        })
    };
    for (tool, args) in [
        (
            "phux_send_keys",
            json!({ "target": "%build", "keys": ["ls"] }),
        ),
        ("phux_paste", json!({ "target": "%build", "text": "ls" })),
    ] {
        let sent = dispatch_against(tool, args, identity_only)
            .await
            .unwrap_or_else(|err| panic!("{tool}: identity-only must resolve: {}", err.0));
        assert_eq!(sent["sent"], true, "{tool}");
        assert_eq!(sent["pane"], "@1", "{tool}");
    }
}

/// ADR-0075 point 3 on the MCP set-valued and placement tools: `%name`
/// reaches `phux_tag`, `phux_kill`, and `phux_spawn` placement through the
/// agent resolver, and a refusal is typed rather than "no such target".
#[tokio::test]
async fn percent_name_reaches_tag_kill_and_spawn_placement() {
    let named = || {
        agent_spec(&AgentRecord {
            name: "build".to_owned(),
            state: AgentMetaState::Working,
            ..AgentRecord::default()
        })
    };
    let tagged = dispatch_against(
        "phux_tag",
        json!({ "action": "add", "target": "%build", "tags": ["ci"] }),
        named,
    )
    .await
    .expect("tag %build");
    assert_eq!(tagged["terminals"][0]["terminal"], "@1");

    let constant = || {
        agent_spec(&AgentRecord {
            name: "claude".to_owned(),
            kind: Some("claude".to_owned()),
            state: AgentMetaState::Working,
            ..AgentRecord::default()
        })
    };
    for (tool, args, want) in [
        (
            "phux_kill",
            json!({ "target": "%claude", "confirm": true }),
            "invalid_agent_name",
        ),
        (
            "phux_tag",
            json!({ "action": "ls", "target": "%claude" }),
            "invalid_agent_name",
        ),
        (
            "phux_spawn",
            json!({ "target": "%claude", "split": "vertical" }),
            "invalid_agent_name",
        ),
        (
            "phux_spawn",
            json!({ "target": "%ghost", "split": "vertical" }),
            "no_such_target",
        ),
    ] {
        let err = dispatch_against(tool, args, constant)
            .await
            .expect_err("a refused name does nothing");
        assert_eq!(contract_code(&err), want, "{tool}: {}", err.0);
    }
}
