//! Golden documents for the in-process tools, and ([`sweep`]) the proof that
//! no in-process tool spawns the CLI.
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
use phux_client::layout::{ArrangementDoc, SplitDir};
use phux_client::layout_ops::{DEFAULT_LAYOUT_GROUP_ID, LayoutMutation, apply_mutation};
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

use crate::cli_adapter::spawn_record;

mod lifecycle;
mod residue;
mod selection;
mod spawn;
mod sweep;

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
    let mut workspace = ArrangementDoc::single(ResourceId::local(panes[0]));
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

/// The layout cases name a projection so their layout stays metadata the
/// scripted server stores; the default layout is the arrangement record.
const PROJECTION: &str = "app.layout/v1/1";

fn with_layout(spec: ScriptSpec, panes: &[u32]) -> ScriptSpec {
    spec.stored_metadata(
        Scope::Group(DEFAULT_LAYOUT_GROUP_ID),
        PROJECTION,
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
        golden: "kill_session_org",
        tool: "phux_kill",
        args: || json!({ "target": "work", "confirm": true }),
        spec: || org_spec(org_fixture().0),
    },
    Case {
        golden: "rm",
        tool: "phux_rm",
        args: || json!({ "target": "parked", "confirm": true }),
        spec: || org_spec(org_fixture().0),
    },
    Case {
        golden: "relaunch",
        tool: "phux_relaunch",
        args: || json!({ "target": format!("^{}", org_fixture().1) }),
        spec: || org_spec(org_fixture().0),
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
        args: || {
            json!({
                "target": "@4",
                "split": "vertical",
                "ratio": 0.3,
                "idempotency_key": "07070707070707070707070707070707",
            })
        },
        spec: spawn::placed_spec,
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
        spec: || {
            let mut fixture = phux_client::testkit::OrgFixture::new();
            fixture.session(
                "work",
                phux_protocol::wire::frame::SessionLifetime::Persistent,
            );
            org_spec(fixture)
        },
    },
    Case {
        golden: "insert_pane",
        tool: "phux_insert_pane",
        args: || {
            json!({
                "target": "@1",
                "new_pane": "@2",
                "direction": "vertical",
                "ratio": 0.3,
                "projection": [PROJECTION],
            })
        },
        spec: || with_layout(ScriptSpec::new().state(state()), &[1]),
    },
    Case {
        golden: "move_pane",
        tool: "phux_move_pane",
        args: || json!({ "source": "@2", "target": "@1", "projection": [PROJECTION] }),
        spec: || with_layout(ScriptSpec::new().state(state()), &[1, 2]),
    },
    Case {
        golden: "swap_pane",
        tool: "phux_swap_pane",
        args: || json!({ "first": "@1", "second": "@2", "projection": [PROJECTION] }),
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

/// An organization fixture: session `work` with a running shell and a
/// completed job, and an empty persistent session `parked`. Returns the
/// fixture and the completed job's key.
fn org_fixture() -> (
    phux_client::testkit::OrgFixture,
    phux_protocol::ids::ResourceKey,
) {
    use phux_protocol::wire::frame::{ExecState, SessionLifetime};
    let mut fixture = phux_client::testkit::OrgFixture::new();
    let work = fixture.session("work", SessionLifetime::Persistent);
    fixture.shell(work);
    let done = fixture.resource(work, Some(ExecState::Completed));
    fixture.session("parked", SessionLifetime::Persistent);
    (fixture, done)
}

/// A scripted server answering from `fixture`.
fn org_spec(fixture: phux_client::testkit::OrgFixture) -> ScriptSpec {
    ScriptSpec::new().organization(fixture)
}

fn golden_path(name: &str) -> PathBuf {
    std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("the test runner sets CARGO_MANIFEST_DIR"),
    )
    .join("tests/golden")
    .join(format!("{name}.json"))
}

/// Run `case` against a scripted server that answers every connection the
/// tool dials with a fresh copy of its spec, and return the canonical JSON
/// document carried by `tools/call` (object key order is not a contract).
async fn run(case: &Case) -> String {
    let dir = tempfile::tempdir().expect("temp dir");
    let (socket, server) = testkit::spawn_serve_every(dir.path(), case.spec);
    let mut args = (case.args)();
    args["socket"] = json!(socket.to_string_lossy());
    let result = crate::tools::dispatch(case.tool, &args).await;
    server.abort();
    let mut value = result.unwrap_or_else(|err| panic!("{} failed: {}", case.golden, err.0));
    crate::contract_tests::validate_output(case.tool, &value);
    value.sort_all_objects();
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
    let (socket, server) = testkit::spawn_serve_every(dir.path(), spec);
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
