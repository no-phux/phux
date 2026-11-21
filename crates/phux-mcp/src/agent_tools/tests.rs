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
