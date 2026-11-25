//! Agent-name selection contracts across input, read, and placement tools.

use phux_client::agent_meta::{AgentMetaState, AgentRecord};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{RESOURCE_AGENT_KEY, Scope};
use serde_json::json;

use super::{agent_spec, contract_code, dispatch_against};

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

    // Two panes sharing a name, as an older server stored a kind name.
    let constant = || {
        let record = AgentRecord {
            name: "claude".to_owned(),
            kind: Some("claude".to_owned()),
            state: AgentMetaState::Working,
            ..AgentRecord::default()
        };
        agent_spec(&record).stored_metadata(
            Scope::Resource(ResourceId::local(2)),
            RESOURCE_AGENT_KEY,
            record.encode(),
        )
    };
    for (tool, args, want) in [
        (
            "phux_kill",
            json!({ "target": "%claude", "confirm": true }),
            "selector_not_single",
        ),
        (
            "phux_tag",
            json!({ "action": "ls", "target": "%claude" }),
            "selector_not_single",
        ),
        (
            "phux_spawn",
            json!({ "target": "%claude", "split": "vertical" }),
            "selector_not_single",
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
