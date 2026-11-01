//! Host-local profile resolution, off the event loop and fenced by stable session.
use super::{denied, json, refusal, spawn};
use crate::runtime::commands::CommandContext;
use phux_plugin::{LaunchMode, resolve_launch};
use phux_protocol::ids::SessionKey;
use phux_protocol::native_agent::{
    NativeIntegration, NativeIntegrationCatalog, NativeRequest, NativeRequestPayload,
    SpawnNativeAgent, validate_payload,
};
use phux_protocol::wire::frame::{CommandResult, ErrorCode};
use std::collections::BTreeMap;
use std::path::Path;

const MAX_INTEGRATIONS: usize = 64;

#[cfg(test)]
mod tests;

pub(super) async fn list() -> CommandResult {
    match tokio::task::spawn_blocking(catalog).await {
        Ok(Ok(catalog)) => json(&catalog),
        Ok(Err(error)) => error,
        Err(error) => refusal(error.to_string()),
    }
}

fn catalog() -> Result<NativeIntegrationCatalog, CommandResult> {
    let config = phux_config::loader::config_path();
    catalog_at(&config)
}

fn catalog_at(config: &Path) -> Result<NativeIntegrationCatalog, CommandResult> {
    let items = phux_plugin::list_launchable(config, LaunchMode::Native)
        .map_err(|e| refusal(e.to_string()))?;
    let mut names = BTreeMap::new();
    // Include built-in profiles even when a broken override suppresses their
    // fallback: a missing button must not conceal a configuration error.
    names.insert("opencode".to_owned(), Some("OpenCode".to_owned()));
    names.insert("fx".to_owned(), Some("fx".to_owned()));
    for item in items {
        names.insert(item.integration_id, item.display_name);
        if names.len() > MAX_INTEGRATIONS {
            return Err(refusal("native integration catalog exceeds 64 profiles"));
        }
    }
    let integrations = names
        .into_iter()
        .map(|(integration, display_name)| {
            let outcome = resolve_launch(
                config,
                &integration,
                &[],
                Path::new("/"),
                LaunchMode::Native,
            );
            NativeIntegration {
                integration,
                display_name,
                native_available: outcome.is_ok(),
                auth_command: outcome.as_ref().ok().and_then(auth_command),
                native_detail: outcome.err().map(|error| error.to_string()),
            }
        })
        .collect();
    let catalog = NativeIntegrationCatalog { integrations };
    validate_payload(&catalog).map_err(refusal)?;
    Ok(catalog)
}

fn auth_command(resolved: &phux_plugin::ResolvedLaunch) -> Option<Vec<String>> {
    let [executable, mode] = resolved.argv.as_slice() else {
        return None;
    };
    if resolved.native.as_ref()?.provider != "opencode"
        || mode != "acp"
        || Path::new(executable).file_name()? != "opencode"
        || phux_plugin::executable_path(executable).is_none()
    {
        return None;
    }
    Some(vec![executable.clone(), "auth".into(), "login".into()])
}

pub(super) async fn launch(
    ctx: &CommandContext,
    integration: String,
    session: SessionKey,
    cwd: String,
) -> CommandResult {
    let request = NativeRequest::LaunchIntegration {
        integration: integration.clone(),
        session,
        cwd: cwd.clone(),
    };
    if let Err(error) = authorize_launch(ctx, &request, &session) {
        return error;
    }
    let resolved = tokio::task::spawn_blocking(move || {
        resolve(&phux_config::loader::config_path(), &integration, &cwd)
    })
    .await;
    let mut spec = match resolved {
        Ok(Ok(spec)) => spec,
        Ok(Err(error)) => return error,
        Err(error) => return refusal(error.to_string()),
    };
    // Config and filesystem work yielded. Recheck authority and resolve the
    // stable key again; a deleted/renamed session must never adopt its old name.
    spec.session = match authorize_launch(ctx, &request, &session) {
        Ok(name) => name,
        Err(error) => return error,
    };
    spawn(ctx, &spec)
}

pub(super) fn authorize_launch(
    ctx: &CommandContext,
    request: &NativeRequest,
    session: &SessionKey,
) -> Result<String, CommandResult> {
    let request = NativeRequestPayload::new(request.clone()).map_err(refusal)?;
    let command = phux_protocol::wire::frame::Command::NativeAgent { request };
    ctx.state.with(|s| {
        if s.native_agents().sealed {
            return Err(CommandResult::Error {
                code: ErrorCode::Overflow,
                message: "daemon upgrade is in progress; no agent was started; reconnect before launching again".into(),
            });
        }
        if s.is_attached_viewer(ctx.client_id)
            || s.connection_grant(ctx.client_id)
                .is_none_or(|grant| grant.kind() != phux_core::actor::PrincipalKind::Human)
            || !matches!(
                crate::policy::authorize_command(s, ctx.client_id, &command),
                Ok(crate::policy::Admission::Run)
            )
        {
            return Err(denied(
                "native integration launch requires human Create+Bind+Input authority",
            ));
        }
        let id = s
            .registry()
            .session_id(&crate::org_wire::core_session_key(session))
            .ok_or_else(|| refusal("native integration session is missing or stale"))?;
        let record = s
            .registry()
            .session(id)
            .ok_or_else(|| refusal("native integration session is missing or stale"))?;
        Ok(record.name.clone())
    })
}

fn valid_directory(cwd: &Path) -> Result<(), CommandResult> {
    if !cwd.is_absolute() || !cwd.is_dir() {
        return Err(refusal(
            "native launch cwd must be an absolute existing directory on the executing host",
        ));
    }
    Ok(())
}

fn resolve(config: &Path, integration: &str, cwd: &str) -> Result<SpawnNativeAgent, CommandResult> {
    valid_directory(Path::new(cwd))?;
    let resolved = resolve_launch(config, integration, &[], Path::new(cwd), LaunchMode::Native)
        .map_err(|error| refusal(error.to_string()))?;
    valid_directory(&resolved.cwd)?;
    let native = resolved
        .native
        .ok_or_else(|| refusal("integration has no native transport"))?;
    Ok(SpawnNativeAgent {
        session: String::new(),
        backend: native.backend.as_str().into(),
        provider: native.provider,
        command: resolved.argv,
        cwd: Some(
            resolved
                .cwd
                .to_str()
                .ok_or_else(|| refusal("native cwd is not UTF-8"))?
                .into(),
        ),
        env: BTreeMap::new(),
        native_id: None,
        guardian: None,
        guardian_key: None,
        guardian_expected: None,
    })
}
