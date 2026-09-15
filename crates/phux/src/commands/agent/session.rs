//! Durable bridge for provider-native agent resume identity.
//!
//! `phux.agent-session/v1` is live, terminal-scoped L3 metadata. It is not the
//! durable store: terminal metadata dies with the Terminal. `workspace save`
//! copies a confirmed record into the versioned archive, and restore stamps the
//! returned replacement Terminal after replaying the provider's native argv.
//!
//! The record type and its wire round trips ([`AgentSessionRecord`],
//! [`persist_record`], [`fetch_record_index`]) live in
//! `phux_client::agent_session_record` and are re-exported here so the rest
//! of the CLI keeps its established `crate::commands::agent::{...}` import
//! path. What stays here is launch-plan resolution
//! ([`prepare`]/[`prepare_for_launch`]), which depends on `phux-plugin`'s
//! `ResolvedLaunch` and so cannot live in the headless client library.

use std::collections::BTreeMap;
use std::path::PathBuf;

use phux_plugin::ResolvedLaunch;

pub(crate) use phux_client::agent_session_record::{
    AgentSessionRecord, fetch_record_index, persist_record,
};

/// A launch carrying one established or resumed provider-native session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedAgentSession {
    pub(crate) record: AgentSessionRecord,
    pub(crate) argv: Vec<String>,
    pub(crate) env: BTreeMap<String, String>,
    pub(crate) cwd: PathBuf,
}

/// Prepare a native session for a new `phux launch`.
///
/// A caller-supplied value in the template's `native_env` means “resume this
/// exact existing provider session.” Without one, providers that document a
/// caller-supplied fresh identity receive a generated UUID through their
/// structured `fresh_args`. Providers without such an API remain ordinary
/// non-restorable fresh launches; callers may still resume a known identity by
/// supplying `native_env`.
pub(crate) fn prepare_for_launch(
    resolved: &ResolvedLaunch,
) -> Result<Option<PreparedAgentSession>, String> {
    let Some(policy) = resolved
        .session_identity
        .as_ref()
        .filter(|policy| policy.supports_native_restore())
    else {
        return Ok(None);
    };
    if let Some(value) = std::env::var_os(&policy.native_env) {
        let native_id = value.into_string().map_err(|_| {
            format!(
                "integration {:?} session identity in {} is not UTF-8",
                resolved.integration_id, policy.native_env
            )
        })?;
        return prepare(resolved, &native_id).map(Some);
    }
    if !policy.supports_native_fresh() {
        return Ok(None);
    }
    let native_id = uuid::Uuid::new_v4().to_string();
    let argv = resolved
        .fresh_argv(&native_id)
        .map_err(|err| format!("integration {:?}: {err}", resolved.integration_id))?;
    prepare_with_argv(resolved, &native_id, argv).map(Some)
}

/// Construct a provider-native resume launch from trusted current template
/// data plus inert archived identity.
pub(crate) fn prepare(
    resolved: &ResolvedLaunch,
    native_id: &str,
) -> Result<PreparedAgentSession, String> {
    let argv = resolved
        .resume_argv(native_id)
        .map_err(|err| format!("integration {:?}: {err}", resolved.integration_id))?;
    prepare_with_argv(resolved, native_id, argv)
}

fn prepare_with_argv(
    resolved: &ResolvedLaunch,
    native_id: &str,
    argv: Vec<String>,
) -> Result<PreparedAgentSession, String> {
    let policy = resolved.session_identity.as_ref().ok_or_else(|| {
        format!(
            "integration {:?} no longer declares session identity",
            resolved.integration_id
        )
    })?;
    let record = AgentSessionRecord::new(&resolved.plugin_id, &resolved.integration_id, native_id)?;
    let mut env = BTreeMap::new();
    env.insert(policy.native_env.clone(), native_id.to_owned());
    Ok(PreparedAgentSession {
        record,
        argv,
        env,
        cwd: resolved.cwd.clone(),
    })
}
