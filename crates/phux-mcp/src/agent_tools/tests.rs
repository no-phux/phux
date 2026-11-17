use std::path::PathBuf;

use tempfile::TempDir;

use super::*;
use crate::cli_adapter::fake;

/// A fake `phux` answering each agent verb with the real CLI's shape,
/// including `wait`'s document-then-exit-124 timeout.
pub(super) fn fake_cli() -> (TempDir, CliAdapter, PathBuf) {
    fake::cli(
        r#"case "$2" in
  set) printf '@1\t{"name":"bot"}\n' ;;
  clear) printf '@1\t-\n' ;;
  wait) printf '{"schema_version":1,"satisfied":false,"baseline":"working","state":"working"}\n'
        exit 124 ;;
  send-keys) printf '{"schema_version":1,"verified":true}\n' ;;
  prompt) printf '{"schema_version":1,"delivery":"ok","transition_observed":false}\n'
          exit 124 ;;
  answer) printf '{"schema_version":1,"delivered":true}\n' ;;
  start) printf '{"schema_version":1,"started":true,"ready":true}\n' ;;
  session) case "$3" in
    open) printf '{"schema_version":1,"resource":"@9","parent":"@7","provider":"claude","native_id":null}\n' ;;
    close) printf '@9\tclosed\n' ;;
  esac ;;
  emit) printf '{"schema_version":1,"resource":"@9","seq":42,"ts_ms":1757404800123,"type":"prompt"}\n' ;;
  log) printf '{"schema_version":1,"resource":"@9","parent":"@7","provider":"claude","native_id":null,"records":[{"seq":1,"ts_ms":10,"type":"session_start","data":{}},{"seq":2,"ts_ms":20,"type":"prompt","data":{"chars":4}}]}\n' ;;
  *) printf '{"schema_version":1,"agents":[]}\n' ;;
esac
"#,
    )
}

pub(super) use fake::assert_argv;

/// Every agent verb is its own tool: none carries an `action`
/// discriminant, and the read and write shapes never merge.
#[test]
fn the_agent_family_is_distinct_tools_with_no_action_multiplexer() {
    let schemas: Vec<Value> = SPECS.iter().map(|spec| (spec.schema)()).collect();
    let names: Vec<&str> = schemas
        .iter()
        .filter_map(|schema| schema["name"].as_str())
        .collect();
    for schema in &schemas {
        assert!(
            schema["inputSchema"]["properties"].get("action").is_none(),
            "{} still multiplexes on `action`",
            schema["name"],
        );
    }
    let props = |name: &str| schemas[names.iter().position(|n| *n == name).unwrap()].clone();
    for read in ["phux_agent_list", "phux_agent_show", "phux_agent_explain"] {
        let schema = props(read);
        for forbidden in ["name", "keys", "until", "state", "timeout_secs"] {
            assert!(
                schema["inputSchema"]["properties"].get(forbidden).is_none(),
                "{read} carries `{forbidden}` from another verb",
            );
        }
    }
    assert!(
        props("phux_agent_wait")["inputSchema"]["properties"]
            .get("name")
            .is_none(),
        "wait must not inherit the identity-declaration arguments",
    );
    assert!(
        props("phux_agent_set")["inputSchema"]["properties"]
            .get("until")
            .is_none(),
        "set must not inherit the wait arguments",
    );
}

/// Validation happens before any subprocess: an adapter pointed at a
/// program that cannot exist proves nothing was executed.
#[tokio::test]
async fn malformed_arguments_are_rejected_before_execution() {
    let adapter = CliAdapter::new("must-not-execute");
    for (name, args) in [
        // Missing required.
        ("phux_agent_send_keys", json!({ "target": "@1" })),
        ("phux_agent_send_keys", json!({ "keys": ["a"] })),
        // Empty key batch: all-or-nothing has nothing to deliver.
        (
            "phux_agent_send_keys",
            json!({ "target": "@1", "keys": [] }),
        ),
        // Closed vocabularies.
        ("phux_agent_wait", json!({ "until": ["unknown"] })),
        ("phux_agent_wait", json!({ "timeout_secs": 0 })),
        ("phux_agent_wait", json!({ "timeout_secs": 3601 })),
        ("phux_agent_wait", json!({ "any": "yes" })),
        ("phux_agent_wait", json!({ "target": "@1", "any": true })),
        ("phux_agent_prompt", json!({ "target": "@1" })),
        (
            "phux_agent_prompt",
            json!({ "target": "@1", "text": "two\nlines" }),
        ),
        (
            "phux_agent_prompt",
            json!({ "target": "@1", "text": "hello", "until": ["unknown"] }),
        ),
        ("phux_agent_answer", json!({ "target": "@1", "id": "q" })),
        (
            "phux_agent_answer",
            json!({ "target": "@1", "id": "q", "choice": 0 }),
        ),
        (
            "phux_agent_answer",
            json!({ "target": "@1", "id": "q", "choice": 1, "text": "yes" }),
        ),
        (
            "phux_agent_answer",
            json!({ "target": "@1", "id": "q", "choice": 1, "allow_unlisted": true }),
        ),
        (
            "phux_agent_start",
            json!({ "name": "bot", "kind": "codex" }),
        ),
        (
            "phux_agent_start",
            json!({ "name": "bot", "kind": "codex", "target": "@1", "force": "yes" }),
        ),
    ] {
        let err = call_with_adapter(name, &args, &adapter)
            .await
            .expect_err(&format!("{name} accepted {args}"));
        assert!(
            !err.0.starts_with("not a CLI-backed tool"),
            "{name} is not CLI-backed; test it in-process: {}",
            err.0,
        );
    }
}

/// The in-process tools refuse a malformed call before dialing: with the
/// socket at a dead path, the error names the argument, not `no_server`.
#[tokio::test]
async fn in_process_tools_reject_malformed_arguments_before_dialing() {
    for (name, mut args, expected) in [
        // Unknown key.
        (
            "phux_agent_list",
            json!({ "target": "@1" }),
            "unknown argument `target`",
        ),
        (
            "phux_agent_show",
            json!({ "until": ["idle"] }),
            "unknown argument `until`",
        ),
        (
            "phux_agent_explain",
            json!({ "name": "bot" }),
            "unknown argument `name`",
        ),
        (
            "phux_agent_clear",
            json!({ "name": "bot" }),
            "unknown argument `name`",
        ),
        // The multiplexer's old shape is not silently accepted.
        (
            "phux_agent_list",
            json!({ "action": "list" }),
            "unknown argument `action`",
        ),
        (
            "phux_agent_show",
            json!({ "action": "show" }),
            "unknown argument `action`",
        ),
        // Missing required.
        (
            "phux_agent_set",
            json!({ "target": "@1" }),
            "missing required argument `name`",
        ),
        // Closed vocabularies.
        (
            "phux_agent_set",
            json!({ "name": "b", "state": "busy" }),
            "state",
        ),
        (
            "phux_agent_set",
            json!({ "name": "b", "attention": "urgent" }),
            "attention",
        ),
    ] {
        args["socket"] = json!("/nonexistent/phux-mcp-agent-tools.sock");
        let err = crate::tools::call_registered(name, &args)
            .await
            .expect_err(&format!("{name} accepted {args}"));
        assert!(
            err.0.contains(expected) && !err.0.contains("no_server"),
            "{name} {args}: {}",
            err.0,
        );
    }
}
