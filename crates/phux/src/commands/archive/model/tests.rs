#![allow(clippy::expect_used, reason = "tests")]

use super::*;

#[test]
fn parses_restore_archive_with_missing_command_and_cwd() {
    let json = r#"{
        "schema_version": 1,
        "sessions": [
            {
                "name": "bench",
                "windows": [
                    {
                        "name": "main",
                        "panes": [
                            { "title": "agent", "cols": 80, "rows": 24 }
                        ]
                    }
                ]
            }
        ]
    }"#;

    let archive = parse_archive(json).expect("archive parses");
    let plan = restore_plan(&archive, &[]).expect("restore plan");

    assert_eq!(plan.creates.len(), 1);
    assert_eq!(plan.creates[0].name, "bench");
    assert_eq!(plan.creates[0].cwd, None);
    assert_eq!(plan.creates[0].command, None);
    assert_eq!(plan.creates[0].agent_session, None);
}

#[test]
fn restore_plan_keeps_host_directory_and_project() {
    let json = r#"{
        "schema_version": 2,
        "sessions": [
            {
                "name": "api",
                "host": "edge",
                "cwd": "/src/api",
                "project": "phux",
                "project_link": "mobile",
                "windows": []
            }
        ]
    }"#;
    let archive = parse_archive(json).expect("archive parses");
    let plan = restore_plan(&archive, &[]).expect("restore plan");
    assert_eq!(plan.creates[0].project_link.as_deref(), Some("mobile"));
    let saved = serde_json::to_value(&archive).expect("archive renders");
    assert_eq!(saved["sessions"][0]["project_link"], "mobile");
    assert_eq!(plan.creates[0].host.as_deref(), Some("edge"));
    assert_eq!(plan.creates[0].cwd.as_deref(), Some("/src/api"));
    assert_eq!(plan.creates[0].project.as_deref(), Some("phux"));
    let lines = phux_client_core::organization::organization_lines(&[
        phux_client_core::organization::OrganizedSession {
            name: plan.creates[0].name.clone(),
            host: plan.creates[0].host.clone(),
            directory: plan.creates[0].cwd.clone(),
            project: plan.creates[0].project.clone(),
        },
    ]);
    assert_eq!(
        lines,
        vec!["api host=edge directory=/src/api project=phux".to_owned()]
    );
}

#[test]
fn restore_plan_uses_active_pane_for_missing_command_and_cwd() {
    let json = r#"{
        "schema_version": 1,
        "sessions": [
            {
                "name": "bench",
                "windows": [
                    {
                        "name": "main",
                        "panes": [
                            { "cwd": "/wrong", "command": ["wrong"] },
                            { "active": true, "cwd": "/right", "command": ["right"] }
                        ]
                    }
                ]
            }
        ]
    }"#;

    let archive = parse_archive(json).expect("archive parses");
    let plan = restore_plan(&archive, &[]).expect("restore plan");

    assert_eq!(plan.creates[0].cwd.as_deref(), Some("/right"));
    assert_eq!(plan.creates[0].command, Some(vec!["right".to_owned()]));
}

#[test]
fn pane_cwd_overrides_session_directory_on_restore() {
    let archive = parse_archive(
        r#"{
        "schema_version": 2,
        "sessions": [{
            "name": "site",
            "cwd": "/checkout",
            "windows": [{
                "name": "web",
                "panes": [{ "active": true, "cwd": "/checkout/web" }]
            }]
        }]
    }"#,
    )
    .expect("archive");
    let plan = restore_plan(&archive, &[]).expect("restore plan");
    let create = &plan.creates[0];
    assert_eq!(create.directory.as_deref(), Some("/checkout"));
    assert_eq!(create.cwd.as_deref(), Some("/checkout/web"));
    let rows = organization_rows(&archive).expect("organization rows");
    assert_eq!(rows[0].directory.as_deref(), Some("/checkout"));
}

#[test]
fn plan_lines_use_the_focused_pane_directory() {
    let json = r#"{
        "schema_version": 2,
        "sessions": [
            {
                "name": "api",
                "active": true,
                "windows": [{
                    "name": "1",
                    "active": true,
                    "panes": [{
                        "active": true,
                        "cwd": "/Users/phall/workspace/phux/crates"
                    }]
                }]
            },
            {
                "name": "zeta",
                "windows": [{
                    "name": "1",
                    "active": true,
                    "panes": [
                        { "cwd": "/wrong" },
                        {
                            "active": true,
                            "cwd": "/Users/phall/workspace/phux/docs"
                        }
                    ]
                }]
            }
        ]
    }"#;
    let archive = parse_archive(json).expect("archive parses");
    let rows = organization_rows(&archive).expect("rows");
    let lines = phux_client_core::organization::organization_lines(&rows);
    assert_eq!(
        lines,
        vec![
            "api host=local directory=/Users/phall/workspace/phux/crates project=-".to_owned(),
            "zeta host=local directory=/Users/phall/workspace/phux/docs project=-".to_owned(),
        ]
    );
}

#[test]
fn restore_plan_carries_only_the_preferred_panes_agent_session() {
    let json = r#"{
        "schema_version": 2,
        "sessions": [{
            "name": "bench",
            "windows": [{
                "name": "main",
                "panes": [
                    {
                        "agent_session": {
                            "plugin_id": "wrong.plugin",
                            "integration_id": "wrong",
                            "native_id": "wrong-session"
                        }
                    },
                    {
                        "active": true,
                        "agent_session": {
                            "plugin_id": "com.phux.agents",
                            "integration_id": "claude-code",
                            "native_id": "session-42"
                        }
                    }
                ]
            }]
        }]
    }"#;

    let archive = parse_archive(json).expect("schema 2 archive parses");
    let plan = restore_plan(&archive, &[]).expect("restore plan");
    let agent = plan.creates[0]
        .agent_session
        .as_ref()
        .expect("preferred agent session");
    assert_eq!(agent.plugin_id, "com.phux.agents");
    assert_eq!(agent.integration_id, "claude-code");
    assert_eq!(agent.native_id, "session-42");
}

#[test]
fn legacy_or_invalid_agent_records_fail_closed() {
    let legacy = r#"{
        "schema_version": 1,
        "sessions": [{
            "name": "bench",
            "windows": [{
                "name": "main",
                "panes": [{
                    "agent_session": {
                        "plugin_id": "com.phux.agents",
                        "integration_id": "claude-code",
                        "native_id": "session-42"
                    }
                }]
            }]
        }]
    }"#;
    assert!(
        parse_archive(legacy)
            .expect_err("schema 1 must not smuggle a schema 2 record")
            .contains("schema 1")
    );

    let invalid = legacy
        .replace("\"schema_version\": 1", "\"schema_version\": 2")
        .replace("\"session-42\"", "\" padded \"");
    assert!(
        parse_archive(&invalid)
            .expect_err("padded identity is untrusted")
            .contains("invalid agent session record")
    );

    let option_shaped = legacy
        .replace("\"schema_version\": 1", "\"schema_version\": 2")
        .replace(
            "\"session-42\"",
            "\"--dangerously-bypass-approvals-and-sandbox\"",
        );
    assert!(
        parse_archive(&option_shaped)
            .expect_err("option-shaped identity is untrusted")
            .contains("invalid agent session record")
    );

    let unknown = legacy
        .replace("\"schema_version\": 1", "\"schema_version\": 2")
        .replace(
            "\"native_id\": \"session-42\"",
            "\"native_id\": \"session-42\", \"argv\": [\"sh\"]",
        );
    assert!(
        parse_archive(&unknown)
            .expect_err("unknown resume authority is untrusted")
            .contains("unknown field")
    );
}

/// Read through [`parse_archive`], the reader `workspace restore` uses.
#[test]
fn frozen_workspace_archive() {
    phux_frozen::custom_json("workspace-archive", |bytes| {
        let text = std::str::from_utf8(bytes).map_err(|error| error.to_string())?;
        let archive = parse_archive(text)?;
        serde_json::to_value(archive).map_err(|error| error.to_string())
    })
    // A restore refuses a guardian: a recipe names no live resource.
    .may_be_absent(&[
        "sessions.windows.panes.native_launch.guardian",
        "sessions.windows.panes.native_launch.guardian_expected",
        "sessions.windows.panes.native_launch.guardian_key",
    ])
    .check();
}
