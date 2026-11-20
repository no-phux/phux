//! Goldens for tools leaving the CLI residue, frozen through the CLI path
//! before each tool moves in-process.
//!
//! A case is a document (pretty, keys sorted, like [`super::run`]) or a
//! refusal, whose golden is the tool error's text verbatim: the CLI's
//! one-line stderr error document, `exit_code` included. The server log path
//! in a transport remedy depends on the environment, so it is spelled
//! `<server-log>` in the golden.
//!
//! Every case's tool has moved in-process and runs on every test run,
//! against the goldens the CLI froze.

use phux_client::agent_meta::{AgentMetaState, AgentRecord};
use phux_client::testkit::{self, ScriptSpec};
use phux_protocol::caps::{ServerFeature, ServerFeatureSet};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{
    RESOURCE_AGENT_KEY, Scope, ServingUser, WHOAMI_KEY, WhoamiRecord,
};
use serde_json::{Value, json};
use tokio::net::UnixListener;

use super::{agent_spec, golden_path, screen, state};

/// A socket nothing listens on, spelled the same on every machine.
const DEAD_SOCKET: &str = "/nonexistent/phux-mcp-golden.sock";

/// What one case pins.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// The tool's result document.
    Document,
    /// The tool error's text.
    Refusal,
}

/// Where the case's `socket` points.
#[derive(Clone, Copy)]
enum Socket {
    /// A scripted server answering every connection with the spec.
    Scripted,
    /// [`DEAD_SOCKET`].
    Dead,
}

/// One residue tool's golden scenario.
struct Case {
    golden: &'static str,
    tool: &'static str,
    args: fn() -> Value,
    spec: fn() -> ScriptSpec,
    socket: Socket,
    expect: Expect,
}

/// `@1` running a working codex declared as `build`.
fn build_agent() -> ScriptSpec {
    agent_spec(&AgentRecord {
        name: "build".to_owned(),
        kind: Some("codex".to_owned()),
        state: AgentMetaState::Working,
        ..AgentRecord::default()
    })
}

/// [`build_agent`] plus `@2` declared blocked as `review`.
fn two_agents() -> ScriptSpec {
    let review = AgentRecord {
        name: "review".to_owned(),
        kind: Some("claude".to_owned()),
        state: AgentMetaState::Blocked,
        ..AgentRecord::default()
    };
    build_agent().stored_metadata(
        Scope::Resource(ResourceId::local(2)),
        RESOURCE_AGENT_KEY,
        review.encode(),
    )
}

fn whoami_record() -> WhoamiRecord {
    WhoamiRecord {
        schema_version: 1,
        principal: None,
        credential_id: None,
        auth_route: "uds".to_owned(),
        peer_uid: Some(501),
        serving_user: ServingUser {
            uid: 501,
            name: Some("me".to_owned()),
        },
        host: "mini".to_owned(),
        server_version: "0.30.0".to_owned(),
        ssh_client: None,
    }
}

fn whoami_spec() -> ScriptSpec {
    ScriptSpec::new()
        .server_features(ServerFeatureSet::with(&[ServerFeature::Whoami]))
        .stored_metadata(
            Scope::Global,
            WHOAMI_KEY,
            serde_json::to_vec(&whoami_record()).expect("record encodes"),
        )
}

fn plain() -> ScriptSpec {
    ScriptSpec::new().state(state()).screen(&screen())
}

const fn document(
    golden: &'static str,
    tool: &'static str,
    args: fn() -> Value,
    spec: fn() -> ScriptSpec,
) -> Case {
    Case {
        golden,
        tool,
        args,
        spec,
        socket: Socket::Scripted,
        expect: Expect::Document,
    }
}

const fn refusal(
    golden: &'static str,
    tool: &'static str,
    args: fn() -> Value,
    spec: fn() -> ScriptSpec,
) -> Case {
    Case {
        golden,
        tool,
        args,
        spec,
        socket: Socket::Scripted,
        expect: Expect::Refusal,
    }
}

const fn no_server(golden: &'static str, tool: &'static str, args: fn() -> Value) -> Case {
    Case {
        golden,
        tool,
        args,
        spec: ScriptSpec::new,
        socket: Socket::Dead,
        expect: Expect::Refusal,
    }
}

const CASES: &[Case] = &[
    document("agent_list", "phux_agent_list", || json!({}), two_agents),
    document(
        "agent_list_filtered",
        "phux_agent_list",
        || json!({ "states": ["blocked"], "query": "review" }),
        two_agents,
    ),
    document("agent_list_plain", "phux_agent_list", || json!({}), plain),
    no_server("agent_list_no_server", "phux_agent_list", || json!({})),
    document(
        "agent_show",
        "phux_agent_show",
        || json!({ "target": "@1" }),
        build_agent,
    ),
    document("agent_show_focused", "phux_agent_show", || json!({}), plain),
    refusal(
        "agent_show_miss",
        "phux_agent_show",
        || json!({ "target": "@99" }),
        build_agent,
    ),
    refusal(
        "agent_show_unknown_name",
        "phux_agent_show",
        || json!({ "target": "%ghost" }),
        build_agent,
    ),
    refusal(
        "agent_show_invalid_selector",
        "phux_agent_show",
        || json!({ "target": "@x" }),
        build_agent,
    ),
    no_server(
        "agent_show_no_server",
        "phux_agent_show",
        || json!({ "target": "@1" }),
    ),
    document(
        "agent_explain",
        "phux_agent_explain",
        || json!({ "target": "%build" }),
        build_agent,
    ),
    refusal(
        "agent_explain_miss",
        "phux_agent_explain",
        || json!({ "target": "@99" }),
        build_agent,
    ),
    document("whoami", "phux_whoami", || json!({}), whoami_spec),
    refusal(
        "whoami_too_old",
        "phux_whoami",
        || json!({}),
        || ScriptSpec::new().server_features(ServerFeatureSet::new()),
    ),
    refusal(
        "whoami_no_record",
        "phux_whoami",
        || json!({}),
        || ScriptSpec::new().server_features(ServerFeatureSet::with(&[ServerFeature::Whoami])),
    ),
    no_server("whoami_no_server", "phux_whoami", || json!({})),
];

/// The environment-dependent server log path, spelled `<server-log>`.
fn normalized(text: &str) -> String {
    let log = phux_config::instance::server_log_path();
    text.replace(&log.display().to_string(), "<server-log>")
}

/// Run `case` through its registered handler and render what it pins.
async fn render(case: &Case) -> String {
    let mut args = (case.args)();
    let dir = tempfile::tempdir().expect("temp dir");
    let server = match case.socket {
        Socket::Dead => {
            args["socket"] = json!(DEAD_SOCKET);
            None
        }
        Socket::Scripted => {
            let socket = dir.path().join("golden.sock");
            let listener = UnixListener::bind(&socket).expect("bind");
            args["socket"] = json!(socket.to_string_lossy());
            Some(tokio::spawn(testkit::serve_every(listener, case.spec)))
        }
    };
    let result = crate::tools::dispatch(case.tool, &args).await;
    if let Some(server) = server {
        server.abort();
    }
    match (case.expect, result) {
        (Expect::Document, Ok(mut value)) => {
            crate::contract_tests::validate_output(case.tool, &value);
            value.sort_all_objects();
            serde_json::to_string_pretty(&value).expect("pretty") + "\n"
        }
        (Expect::Refusal, Err(err)) => normalized(&err.0) + "\n",
        (Expect::Document, Err(err)) => panic!("{}: expected a document: {}", case.golden, err.0),
        (Expect::Refusal, Ok(value)) => panic!("{}: expected a refusal: {value}", case.golden),
    }
}

/// Compare each of `cases` with its golden.
async fn check(cases: impl Iterator<Item = &Case>) {
    let mut mismatches = Vec::new();
    for case in cases {
        let actual = render(case).await;
        let file = golden_path(case.golden);
        match std::fs::read_to_string(&file) {
            Ok(expected) if expected == actual => {}
            Ok(expected) => mismatches.push(format!(
                "{}: drifted\n--- golden\n{expected}--- actual\n{actual}",
                case.golden
            )),
            Err(_) => mismatches.push(format!(
                "{}: no golden at {}; it is\n{actual}",
                case.golden,
                file.display()
            )),
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

/// A tool that left the residue still answers byte for byte what the CLI
/// answered, and reaching it spawns nothing.
#[tokio::test]
async fn moved_tools_match_the_goldens_the_cli_froze() {
    let _ = crate::cli_adapter::spawn_record::take();
    check(CASES.iter()).await;
    assert!(
        crate::cli_adapter::spawn_record::take().is_empty(),
        "a moved tool spawned the CLI"
    );
}
