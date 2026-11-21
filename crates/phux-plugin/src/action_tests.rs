#![allow(clippy::expect_used, reason = "fixture assertions")]

use super::*;

use serde_json::{Value, json};

fn fixture(platform: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("config.toml"),
        "[[plugins]]\nmanifest = 'phux-plugin.toml'\n",
    )
    .expect("config");
    std::fs::write(
        dir.path().join("phux-plugin.toml"),
        format!(
            r#"
id = "test.actions"
name = "Test Actions"
version = "0.1.0"
min_phux_version = "0.0.2"
{platform}

[[actions]]
id = "context"
title = "Echo context"
command = ["/bin/sh", "-c", 'test "$PHUX_SOCKET" = /dev/null/phux-action.sock || exit 42; test -z "$PHUX_TERMINAL_ID$PHUX_WS_TOKENS$PHUX_WS_TLS_KEY" || exit 43; printf "%s" "$PHUX_ACTION_CONTEXT"']
"#
        ),
    )
    .expect("manifest");
    dir
}

fn request(context: ActionContext) -> PluginActionRequest {
    PluginActionRequest {
        plugin_id: "test.actions".into(),
        action_id: "context".into(),
        timeout: Some(Duration::from_secs(5)),
        cwd: None,
        context,
    }
}

#[tokio::test]
async fn sends_only_the_captured_invocation_context() {
    let dir = fixture("");
    let target = ActionTarget {
        host: Some("hub".into()),
        satellite: Some("worker".into()),
        session: Some("session-key".into()),
        resource_handle: Some("worker/@7".into()),
        entry: Some("entry-key".into()),
        ..ActionTarget::default()
    };
    let output = run_configured_action(
        &dir.path().join("config.toml"),
        &request(ActionContext::observed(
            ActionSource::TuiKey,
            Some(19),
            target,
        )),
    )
    .await
    .expect("child output");
    assert_eq!(output.exit_code, Some(0));
    assert_eq!(
        serde_json::from_str::<Value>(&output.stdout).expect("context"),
        json!({
            "schema_version": 1,
            "source": "tui_key",
            "executor": "invoking_machine",
            "observed": true,
            "client": 19,
            "target": {
                "host": "hub",
                "satellite": "worker",
                "session": "session-key",
                "resource_handle": "worker/@7",
                "entry": "entry-key"
            },
            "input": null
        })
    );
}

#[tokio::test]
async fn refuses_oversized_context_before_starting_the_child() {
    let dir = fixture("");
    let result = run_configured_action(
        &dir.path().join("config.toml"),
        &request(ActionContext::supplied(
            ActionSource::Cli,
            ActionTarget::default(),
            json!("x".repeat(16 * 1024)),
        )),
    )
    .await;
    assert!(matches!(result, Err(PluginActionError::ContextTooLarge)));
}

#[tokio::test]
async fn refuses_an_action_unavailable_on_the_execution_platform() {
    let dir = fixture("platforms = ['windows']");
    let config = dir.path().join("config.toml");
    assert!(configured_actions(&config).expect("catalog").is_empty());
    let result = run_configured_action(
        &config,
        &request(ActionContext::supplied(
            ActionSource::Mcp,
            ActionTarget::default(),
            Value::Null,
        )),
    )
    .await;
    assert!(matches!(
        result,
        Err(PluginActionError::UnsupportedPlatform)
    ));
}
