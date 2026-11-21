use super::super::tests::{assert_argv, fake_cli};
use super::*;

/// The turn-driving tools execute the exact canonical argv.
#[tokio::test]
async fn turn_driving_tools_execute_the_exact_canonical_argv() {
    let (_temp, adapter, log) = fake_cli();
    let prompt = assert_argv(
        &adapter,
        &log,
        "phux_agent_prompt",
        json!({
            "target": "%bot", "text": "Review the patch",
            "expect_agent": "bot", "expect_kind": "codex",
            "until": ["blocked", "done"], "timeout_secs": 45,
            "socket": "/sock"
        }),
        &[
            "agent",
            "prompt",
            "--wait",
            "--expect-agent",
            "bot",
            "--expect-kind",
            "codex",
            "--until",
            "blocked",
            "--until",
            "done",
            "--timeout",
            "45",
            "--json",
            "--socket",
            "/sock",
            "--",
            "%bot",
            "Review the patch",
        ],
    )
    .await;
    assert_eq!(prompt["delivery"], "ok");
    assert_eq!(prompt["transition_observed"], false);

    assert_argv(
        &adapter,
        &log,
        "phux_agent_answer",
        json!({
            "target": "%bot", "id": "deploy", "text": "Proceed",
            "allow_unlisted": true, "socket": "/sock"
        }),
        &[
            "agent",
            "answer",
            "--id",
            "deploy",
            "--text",
            "Proceed",
            "--allow-unlisted",
            "--json",
            "--socket",
            "/sock",
            "--",
            "%bot",
        ],
    )
    .await;

    assert_argv(
        &adapter,
        &log,
        "phux_agent_start",
        json!({
            "name": "reviewer", "kind": "codex", "target": "@8",
            "integration": "codex-cli", "timeout_secs": 40, "force": true,
            "args": ["--model", "gpt-5"], "socket": "/sock"
        }),
        &[
            "agent",
            "start",
            "--kind",
            "codex",
            "--target",
            "@8",
            "--integration",
            "codex-cli",
            "--timeout",
            "40",
            "--force",
            "--json",
            "--socket",
            "/sock",
            "reviewer",
            "--",
            "--model",
            "gpt-5",
        ],
    )
    .await;
}

/// The wait tool always bounds the CLI (which defaults to unbounded),
/// passes each `until` as its own flag, and returns the timeout
/// document rather than collapsing exit 124 into an error.
#[tokio::test]
async fn wait_bounds_the_deadline_and_returns_the_timeout_document() {
    let (_temp, adapter, log) = fake_cli();

    let bounded = assert_argv(
        &adapter,
        &log,
        "phux_agent_wait",
        json!({ "target": "@7", "until": ["blocked", "done"], "timeout_secs": 30, "socket": "/sock" }),
        &[
            "agent", "wait", "--until", "blocked", "--until", "done", "--timeout", "30",
            "--json", "--socket", "/sock", "--", "@7",
        ],
    )
    .await;
    assert_eq!(
        bounded["satisfied"],
        json!(false),
        "exit 124 must arrive as `satisfied: false`, not as a tool error",
    );
    assert_eq!(bounded["baseline"], json!("working"));

    // Omitting `timeout_secs` still bounds the call.
    assert_argv(
        &adapter,
        &log,
        "phux_agent_wait",
        json!({}),
        &[
            "agent",
            "wait",
            "--timeout",
            &WAIT_DEFAULT_TIMEOUT_SECS.to_string(),
            "--json",
        ],
    )
    .await;

    assert_argv(
        &adapter,
        &log,
        "phux_agent_wait",
        json!({ "any": true, "until": ["blocked"], "timeout_secs": 20 }),
        &[
            "agent",
            "wait",
            "--any",
            "--until",
            "blocked",
            "--timeout",
            "20",
            "--json",
        ],
    )
    .await;
}
