//! Bridge a pane's live `AgentSession` into its archived resume record
//! (ADR-0151).
//!
//! `phux launch` stamps `phux.agent-session/v1` on the Terminal it spawns
//! (ADR-0068). An agent started inside an existing shell (Claude Code through
//! the hook shim, pi, omp, `OpenCode`) carries no such record; it opens an
//! `AgentSession` child with `--provider` and `--native-id` instead (ADR-0103).
//! Save turns that child into the same inert record when one enabled
//! integration claims the provider as its `[agent_identity] kind` and declares
//! native resume. Only the owner ids and the opaque native id are archived;
//! restore still rebuilds argv from the current integration.

use std::collections::HashMap;

use phux_client::agent_session_record::AgentSessionRecord;
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};

/// The enabled integration that owns resume for one provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResumeOwner {
    pub(super) plugin_id: String,
    pub(super) integration_id: String,
}

/// A pane's unique live `AgentSession` that names a provider-native id.
struct LiveSession<'a> {
    provider: &'a str,
    native_id: &'a str,
}

/// Merge each local pane's live `AgentSession` into `recorded` (the
/// `phux.agent-session/v1` records read from the server).
///
/// `resolve` maps a provider slug to its resume owner, or says why it has
/// none; it is called at most once per provider. A pane with a launch record
/// keeps it, except that a live session resolving to the same owner refreshes
/// its native id (the provider moved to a new conversation in the same pane).
/// A pane whose live session cannot be bridged and has no record yields a
/// warning: it will restore as a plain shell.
pub(super) fn bridge_live_agent_sessions(
    snapshot: &SessionSnapshot,
    mut recorded: HashMap<ResourceId, AgentSessionRecord>,
    mut resolve: impl FnMut(&str) -> Result<ResumeOwner, String>,
) -> (HashMap<ResourceId, AgentSessionRecord>, Vec<String>) {
    let mut owners: HashMap<String, Result<ResumeOwner, String>> = HashMap::new();
    let mut warnings = Vec::new();
    for pane in phux_client::resource::terminals(snapshot) {
        if !matches!(pane.id, ResourceId::Local { .. }) {
            continue;
        }
        let Some(live) = unique_live_session(snapshot, pane) else {
            continue;
        };
        let owner = owners
            .entry(live.provider.to_owned())
            .or_insert_with(|| resolve(live.provider));
        let existing = recorded.get(&pane.id);
        match bridged_record(existing, &live, owner) {
            Ok(Some(record)) => {
                recorded.insert(pane.id.clone(), record);
            }
            Err(reason) if existing.is_none() => warnings.push(format!(
                "workspace save: pane {}'s {} agent session ({}) will restore as a plain \
                 shell: {reason}",
                phux_client::selector::format_terminal_id(&pane.id),
                live.provider,
                live.native_id,
            )),
            Ok(None) | Err(_) => {}
        }
    }
    (recorded, warnings)
}

/// The pane's only `AgentSession` child, when it names a native id. Several
/// children are ambiguous, the same rule `agent list` applies (ADR-0103).
fn unique_live_session<'a>(
    snapshot: &'a SessionSnapshot,
    pane: &'a ResourceInfo,
) -> Option<LiveSession<'a>> {
    let mut children = phux_client::resource::children_of(snapshot, &pane.id);
    let (Some(child), None) = (children.next(), children.next()) else {
        return None;
    };
    let facet = child.agent.as_ref()?;
    Some(LiveSession {
        provider: facet.provider.as_str(),
        native_id: facet.native_id.as_deref()?,
    })
}

/// The record to archive for one live session, `None` to keep `existing`
/// unchanged, or why nothing resumable could be derived.
fn bridged_record(
    existing: Option<&AgentSessionRecord>,
    live: &LiveSession<'_>,
    owner: &Result<ResumeOwner, String>,
) -> Result<Option<AgentSessionRecord>, String> {
    let owner = owner.as_ref().map_err(Clone::clone)?;
    if let Some(record) = existing {
        let same_owner =
            record.plugin_id == owner.plugin_id && record.integration_id == owner.integration_id;
        if !same_owner || record.native_id == live.native_id {
            return Ok(None);
        }
    }
    AgentSessionRecord::new(&owner.plugin_id, &owner.integration_id, live.native_id).map(Some)
}

/// Resolve `provider` as a detection kind to the enabled integration that
/// claims it (the `phux agent start --kind` rule), and require that it
/// declares provider-native resume.
pub(super) fn resolve_resume_owner(provider: &str) -> Result<ResumeOwner, String> {
    // Only ownership and policy are read; the launch cwd is never used.
    let resolved = phux_plugin::resolve_launch_for_kind(
        &phux_config::loader::config_path(),
        None,
        provider,
        &[],
        std::path::Path::new("/"),
    )
    .map_err(|err| err.to_string())?;
    let resumable = resolved
        .session_identity
        .as_ref()
        .is_some_and(phux_config::integration::IntegrationSessionIdentity::supports_native_restore);
    if !resumable {
        return Err(format!(
            "integration {:?} declares no provider-native resume",
            resolved.integration_id
        ));
    }
    Ok(ResumeOwner {
        plugin_id: resolved.plugin_id,
        integration_id: resolved.integration_id,
    })
}

#[cfg(test)]
mod tests {
    use phux_protocol::ids::{ResourceKind, SessionId, WindowId};
    use phux_protocol::wire::info::AgentFacet;

    use super::*;

    fn pane(id: u32) -> ResourceInfo {
        ResourceInfo::new(ResourceId::local(id), WindowId::new(1), 80, 24)
    }

    fn agent(id: u32, parent: u32, provider: &str, native_id: Option<&str>) -> ResourceInfo {
        let mut info = ResourceInfo::resource(ResourceId::local(id), ResourceKind::AgentSession);
        info.parent = Some(ResourceId::local(parent));
        info.agent =
            Some(AgentFacet::new(provider, "working").with_native_id(native_id.map(str::to_owned)));
        info
    }

    fn snapshot(resources: Vec<ResourceInfo>) -> SessionSnapshot {
        SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
            .with_resources(resources)
    }

    fn claude_owner(provider: &str) -> Result<ResumeOwner, String> {
        match provider {
            "claude" => Ok(ResumeOwner {
                plugin_id: "com.phux.demo.agent-tools".to_owned(),
                integration_id: "claude-code".to_owned(),
            }),
            other => Err(format!("no enabled integration claims {other:?}")),
        }
    }

    fn record(integration: &str, native_id: &str) -> AgentSessionRecord {
        AgentSessionRecord::new("com.phux.demo.agent-tools", integration, native_id)
            .expect("valid record")
    }

    #[test]
    fn an_integration_started_agent_gains_a_resume_record() {
        let snap = snapshot(vec![
            pane(1),
            agent(2, 1, "claude", Some("sess-1")),
            pane(3),
        ]);
        let (records, warnings) = bridge_live_agent_sessions(&snap, HashMap::new(), claude_owner);
        assert_eq!(
            records.get(&ResourceId::local(1)),
            Some(&record("claude-code", "sess-1"))
        );
        assert_eq!(records.len(), 1, "a pane with no agent stays recordless");
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn an_unresumable_provider_warns_and_archives_nothing() {
        let snap = snapshot(vec![pane(1), agent(2, 1, "mystery", Some("abc"))]);
        let (records, warnings) = bridge_live_agent_sessions(&snap, HashMap::new(), claude_owner);
        assert!(records.is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].contains("@1") && warnings[0].contains("plain shell"),
            "{warnings:?}"
        );
    }

    #[test]
    fn sessions_without_a_native_id_or_with_siblings_are_not_bridged() {
        let snap = snapshot(vec![
            pane(1),
            agent(2, 1, "claude", None),
            pane(3),
            agent(4, 3, "claude", Some("a")),
            agent(5, 3, "claude", Some("b")),
        ]);
        let (records, warnings) = bridge_live_agent_sessions(&snap, HashMap::new(), claude_owner);
        assert!(records.is_empty(), "{records:?}");
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn a_launch_record_wins_unless_the_same_owner_moved_on() {
        let snap = snapshot(vec![
            pane(1),
            agent(2, 1, "claude", Some("after-clear")),
            pane(3),
            agent(4, 3, "claude", Some("other")),
        ]);
        let recorded = HashMap::from([
            (ResourceId::local(1), record("claude-code", "launched")),
            (ResourceId::local(3), record("codex", "thread-9")),
        ]);
        let (records, warnings) = bridge_live_agent_sessions(&snap, recorded, claude_owner);
        assert_eq!(
            records.get(&ResourceId::local(1)),
            Some(&record("claude-code", "after-clear")),
            "the same owner's live id is the current conversation"
        );
        assert_eq!(
            records.get(&ResourceId::local(3)),
            Some(&record("codex", "thread-9")),
            "a record for a different owner is never rewritten"
        );
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn an_option_shaped_native_id_is_refused_not_archived() {
        let snap = snapshot(vec![pane(1), agent(2, 1, "claude", Some("--evil"))]);
        let (records, warnings) = bridge_live_agent_sessions(&snap, HashMap::new(), claude_owner);
        assert!(records.is_empty());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
    }

    #[test]
    fn each_provider_is_resolved_once() {
        let snap = snapshot(vec![
            pane(1),
            agent(2, 1, "claude", Some("a")),
            pane(3),
            agent(4, 3, "claude", Some("b")),
        ]);
        let mut calls = 0;
        let (records, _) = bridge_live_agent_sessions(&snap, HashMap::new(), |provider| {
            calls += 1;
            claude_owner(provider)
        });
        assert_eq!(records.len(), 2);
        assert_eq!(calls, 1);
    }
}
