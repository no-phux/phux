use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

pub(crate) const ARCHIVE_SCHEMA_VERSION: u8 = 2;
const LEGACY_ARCHIVE_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct WorkspaceArchive {
    pub(crate) schema_version: u8,
    pub(crate) sessions: Vec<WorkspaceSession>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct WorkspaceSession {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) command: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) windows: Vec<WorkspaceWindow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct WorkspaceWindow {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) layout: Option<WorkspaceLayoutNode>,
    #[serde(default)]
    pub(crate) panes: Vec<WorkspacePane>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkspacePane {
    #[serde(default)]
    pub(crate) active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) command: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) agent_session: Option<WorkspaceAgentSession>,
    /// Extra environment for this pane's process. Written by a project
    /// recipe (ADR-0152); `workspace save` never captures a live pane's env.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) env: BTreeMap<String, String>,
    #[serde(default)]
    pub(crate) cols: u16,
    #[serde(default)]
    pub(crate) rows: u16,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::struct_field_names,
    reason = "the archive mirrors the versioned L3 provenance schema"
)]
pub(crate) struct WorkspaceAgentSession {
    pub(crate) plugin_id: String,
    pub(crate) integration_id: String,
    pub(crate) native_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum WorkspaceLayoutNode {
    Pane {
        pane: usize,
    },
    Split {
        dir: WorkspaceSplitDir,
        ratio: f32,
        left: Box<Self>,
        right: Box<Self>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkspaceSplitDir {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RestoreSummary {
    pub(crate) schema_version: u8,
    pub(crate) restored: Vec<String>,
    pub(crate) skipped_existing: Vec<String>,
    /// Sessions whose restore failed partway through. Each one was rolled
    /// back (every pane created for it so far, killed) before moving on to
    /// the next session (PHA-406 L18 review item 5): a failure never leaves
    /// a half-restored session behind, and never aborts the rest of the
    /// archive.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) failed: Vec<FailedRestore>,
    /// Non-fatal notices against an otherwise-restored session — today,
    /// only "an archived native agent session could not be resumed; this
    /// pane got a plain shell instead" (PHA-406 L18 review item 6).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) warnings: Vec<RestoreWarning>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct FailedRestore {
    pub(crate) name: String,
    pub(crate) reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RestoreWarning {
    pub(crate) session: String,
    pub(crate) message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RestorePlan {
    pub(crate) creates: Vec<CreateRequest>,
    pub(crate) skipped_existing: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CreateRequest {
    pub(crate) name: String,
    pub(crate) cwd: Option<String>,
    pub(crate) command: Option<Vec<String>>,
    pub(crate) agent_session: Option<WorkspaceAgentSession>,
    /// The seed pane's extra environment (see [`WorkspacePane::env`]).
    pub(crate) env: BTreeMap<String, String>,
    /// `(window index, pane index)` into [`Self::windows`] of the pane whose
    /// `cwd`/`command`/`agent_session` seeded the fields above — the one
    /// pane the session-create call itself produces. `None` when the
    /// session has no panes at all.
    pub(crate) seed_position: Option<(usize, usize)>,
    /// The full archived window/pane structure (ADR-0129): every other
    /// pane here is spawned and placed to replay the captured split tree
    /// after the seed pane exists. Empty for a schema-1 archive or a
    /// session with no windows.
    pub(crate) windows: Vec<WorkspaceWindow>,
}

pub(crate) fn parse_archive(input: &str) -> Result<WorkspaceArchive, String> {
    let archive: WorkspaceArchive = serde_json::from_str(input)
        .map_err(|err| format!("invalid workspace archive JSON: {err}"))?;
    if !matches!(
        archive.schema_version,
        LEGACY_ARCHIVE_SCHEMA_VERSION | ARCHIVE_SCHEMA_VERSION
    ) {
        return Err(format!(
            "unsupported workspace archive schema {}; expected {LEGACY_ARCHIVE_SCHEMA_VERSION} or {ARCHIVE_SCHEMA_VERSION}",
            archive.schema_version
        ));
    }
    validate_archive(&archive)?;
    Ok(archive)
}

pub(crate) fn restore_plan(
    archive: &WorkspaceArchive,
    existing_sessions: &[String],
) -> Result<RestorePlan, String> {
    validate_archive(archive)?;
    let existing: BTreeSet<&str> = existing_sessions.iter().map(String::as_str).collect();
    let mut creates = Vec::new();
    let mut skipped_existing = Vec::new();
    for session in &archive.sessions {
        if existing.contains(session.name.as_str()) {
            skipped_existing.push(session.name.clone());
            continue;
        }
        let seed_position = preferred_pane_position(session);
        let pane = seed_position.map(|(wi, pi)| &session.windows[wi].panes[pi]);
        creates.push(CreateRequest {
            name: session.name.clone(),
            cwd: session
                .cwd
                .clone()
                .or_else(|| pane.and_then(|pane| pane.cwd.clone())),
            command: session
                .command
                .clone()
                .or_else(|| pane.and_then(|pane| pane.command.clone())),
            agent_session: pane.and_then(|pane| pane.agent_session.clone()),
            env: pane.map(|pane| pane.env.clone()).unwrap_or_default(),
            seed_position,
            windows: session.windows.clone(),
        });
    }
    Ok(RestorePlan {
        creates,
        skipped_existing,
    })
}

fn validate_archive(archive: &WorkspaceArchive) -> Result<(), String> {
    let mut names = BTreeSet::new();
    if archive.schema_version == LEGACY_ARCHIVE_SCHEMA_VERSION
        && archive.sessions.iter().any(|session| {
            session
                .windows
                .iter()
                .flat_map(|window| &window.panes)
                .any(|pane| pane.agent_session.is_some())
        })
    {
        return Err(
            "workspace archive schema 1 cannot contain native agent session records".to_owned(),
        );
    }
    for session in &archive.sessions {
        if session.name.trim().is_empty() {
            return Err("workspace archive contains a session with an empty name".to_owned());
        }
        if !names.insert(&session.name) {
            return Err(format!(
                "workspace archive contains duplicate session '{}'",
                session.name
            ));
        }
        for pane in session.windows.iter().flat_map(|window| &window.panes) {
            if let Some(agent) = &pane.agent_session {
                phux_client::agent_session_record::AgentSessionRecord::new(
                    &agent.plugin_id,
                    &agent.integration_id,
                    &agent.native_id,
                )
                .map_err(|err| {
                    format!(
                        "workspace archive session '{}' has an invalid agent session record: {err}",
                        session.name
                    )
                })?;
            }
        }
    }
    Ok(())
}

/// `(window index, pane index)` of the pane that seeds a session-create
/// call: the active window's active pane, else its first pane, else any
/// window's active pane, else the very first pane in the session. `None`
/// when the session has no panes at all.
fn preferred_pane_position(session: &WorkspaceSession) -> Option<(usize, usize)> {
    session
        .windows
        .iter()
        .enumerate()
        .find(|(_, window)| window.active)
        .and_then(|(window_index, window)| {
            window
                .panes
                .iter()
                .position(|pane| pane.active)
                .or(if window.panes.is_empty() {
                    None
                } else {
                    Some(0)
                })
                .map(|pane_index| (window_index, pane_index))
        })
        .or_else(|| {
            session.windows.iter().enumerate().find_map(|(wi, window)| {
                window
                    .panes
                    .iter()
                    .position(|pane| pane.active)
                    .map(|pi| (wi, pi))
            })
        })
        .or_else(|| first_pane_position(session))
}

fn first_pane_position(session: &WorkspaceSession) -> Option<(usize, usize)> {
    session
        .windows
        .iter()
        .enumerate()
        .find_map(|(wi, window)| (!window.panes.is_empty()).then_some((wi, 0)))
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
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
}
