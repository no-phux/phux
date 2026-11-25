//! Archived intent is inert until a current host template or an exact owner decision admits it.
use crate::org_wire as ow;
use crate::runtime::commands::CommandContext;
use crate::state::ServerState;
use phux_core::{LaunchSpec, Refusal};
use phux_protocol::ids::{OperationId, ResourceKey, Revision};
use phux_protocol::native_agent::relaunch::RelaunchApproval;
use phux_protocol::wire::frame::{Command, CommandResult};

fn refused(reason: impl Into<String>) -> CommandResult {
    CommandResult::Refused(ow::refusal(&Refusal::InvalidArgument {
        field: "archived_recipe",
        reason: reason.into(),
    }))
}

fn owner(s: &ServerState, client: crate::state::ClientId) -> bool {
    s.clients()
        .peer_identity(client)
        .is_some_and(|peer| peer.transport == phux_protocol::policy::TransportType::UnixSocket)
        && s.connection_grant(client).is_some_and(|grant| {
            grant.is_owner()
                && grant.kind() == phux_core::actor::PrincipalKind::Human
                && grant.on_behalf_of().is_none()
                && grant.revocation().is_none()
        })
        && s.bearer_admission(client).is_none()
}

pub(in crate::runtime) fn prepare(
    ctx: &CommandContext,
    operation: OperationId,
    resource: ResourceKey,
    expected: Revision,
) -> CommandResult {
    let prepared = ctx.state.with_mut(|s| {
        let proposal = proposal(s, ctx.client_id, operation, resource, expected)?;
        let document = serde_json::to_string(&proposal).map_err(|error| refused(error.to_string()))?;
        let carrier_bytes = r#"{"type":"approve_relaunch","approval":}"#.len();
        if document.len().saturating_add(carrier_bytes) > phux_protocol::native_agent::MAX_NATIVE_PAYLOAD_BYTES {
            return Err(CommandResult::error(phux_protocol::wire::frame::ErrorCode::Unsupported,
                "the complete exact raw recipe exceeds this peer's bounded approval payload; nothing was approved"));
        }
        s.native_agents_mut().relaunch_proposals.insert(ctx.client_id, proposal);
        Ok::<_, CommandResult>(document)
    });
    match prepared {
        Ok(document) => {
            CommandResult::OkWith(phux_protocol::wire::frame::CommandValue::Json(document))
        }
        Err(error) => error,
    }
}

fn proposal(
    s: &ServerState,
    client: crate::state::ClientId,
    operation: OperationId,
    resource: ResourceKey,
    expected: Revision,
) -> Result<RelaunchApproval, CommandResult> {
    if !owner(s, client) {
        return Err(refused(
            "raw recipe approval requires the non-delegated local owner socket",
        ));
    }
    let record = s
        .org()
        .resource(&ow::core_resource_key(&resource))
        .ok_or_else(|| refused("archived target is missing"))?;
    if record.revision != ow::core_revision(expected) {
        return Err(CommandResult::Refused(ow::refusal(&Refusal::Conflict {
            current: record.revision,
        })));
    }
    let launch = record
        .launch
        .as_ref()
        .ok_or_else(|| refused("target has no saved recipe"))?;
    let recipe = serde_json::to_value(launch).map_err(|error| refused(error.to_string()))?;
    let execution = execution(s, launch)?;
    let activation =
        serde_json::to_value(&record.activation).map_err(|error| refused(error.to_string()))?;
    Ok(RelaunchApproval {
        instance: *s.idspace.instance().as_bytes(),
        operation,
        resource,
        expected,
        activation,
        recipe,
        execution,
    })
}

fn execution(s: &ServerState, launch: &LaunchSpec) -> Result<serde_json::Value, CommandResult> {
    let mut exact = launch.clone();
    if let LaunchSpec::Terminal(terminal) = &mut exact
        && !terminal
            .env
            .iter()
            .any(|(key, _)| key == phux_protocol::native_agent::NATIVE_HOST_SPEC_ENV)
    {
        let command = crate::runtime::apps::terminal::launch::spec_command(s, terminal);
        let text = |value: &std::ffi::OsStr| {
            value.to_str().map(str::to_owned).ok_or_else(|| {
                refused("raw approval cannot display non-UTF-8 executable input losslessly")
            })
        };
        terminal.argv = command
            .get_argv()
            .iter()
            .map(|value| text(value))
            .collect::<Result<_, _>>()?;
        terminal.cwd = command.get_cwd().map(std::path::PathBuf::from);
        terminal.last_cwd = None;
        terminal.env = command
            .environment()
            .iter()
            .map(|(key, value)| Ok((text(key)?, text(value)?)))
            .collect::<Result<_, CommandResult>>()?;
    }
    serde_json::to_value(exact).map_err(|error| refused(error.to_string()))
}

pub(in crate::runtime) fn approve(
    ctx: &CommandContext,
    approval: &RelaunchApproval,
) -> CommandResult {
    let checked = ctx.state.with(|s| {
        if !owner(s, ctx.client_id) || s.native_agents().relaunch_proposals.get(&ctx.client_id) != Some(approval) {
            return Err(refused("approval was not prepared on this live local-owner connection, or its operation/content changed"));
        }
        if let Some(receipt) = s.org().receipt(&ow::core_operation(&approval.operation)) {
            return Err(if matches!(&receipt.op, phux_core::Op::ResourceRelaunch { resource, expected, .. }
                if *resource == ow::core_resource_key(&approval.resource) && *expected == ow::core_revision(approval.expected)) {
                CommandResult::Accepted { operation: approval.operation }
            } else {
                CommandResult::Refused(ow::refusal(&Refusal::DuplicateOperation))
            });
        }
        let current = proposal(s, ctx.client_id, approval.operation, approval.resource, approval.expected)?;
        if current != *approval { return Err(refused("approval does not match the current host, operation, revision and complete recipe")); }
        let command = Command::ResourceRelaunch { operation: approval.operation, resource: approval.resource, expected: approval.expected };
        if !matches!(crate::policy::authorize_command(s, ctx.client_id, &command), Ok(crate::policy::Admission::Run)) {
            return Err(refused("relaunch authority is no longer current"));
        }
        serde_json::from_value::<LaunchSpec>(approval.execution.clone()).map_err(|error| refused(error.to_string()))
    });
    let launch = match checked {
        Ok(launch) => launch,
        Err(error) => return error,
    };
    if let Some(result) = crate::runtime::commands::native_agent::claim_approved_relaunch(
        ctx,
        approval.operation,
        &approval.resource,
        approval.expected,
    ) {
        return result;
    }
    crate::state::org_operation::relaunch_admitted(
        &ctx.state,
        ctx.client_id,
        phux_core::Command {
            operation: ow::core_operation(&approval.operation),
            op: phux_core::Op::ResourceRelaunch {
                resource: ow::core_resource_key(&approval.resource),
                expected: ow::core_revision(approval.expected),
                launch: Some(launch),
            },
        },
        &ctx.root_token,
    )
}

pub(super) fn relaunch_op(
    s: &ServerState,
    operation: OperationId,
    resource: ResourceKey,
    expected: Revision,
) -> Result<phux_core::Op, Refusal> {
    if let Some(receipt) = s.org().receipt(&ow::core_operation(&operation)) {
        return match &receipt.op {
            phux_core::Op::ResourceRelaunch {
                resource: target,
                expected: revision,
                ..
            } if *target == ow::core_resource_key(&resource)
                && *revision == ow::core_revision(expected) =>
            {
                Ok(receipt.op.clone())
            }
            _ => Err(Refusal::DuplicateOperation),
        };
    }
    terminal(
        s,
        &ow::core_resource_key(&resource),
        ow::core_revision(expected),
    )
    .map(|launch| phux_core::Op::ResourceRelaunch {
        resource: ow::core_resource_key(&resource),
        expected: ow::core_revision(expected),
        launch,
    })
}

/// Only current template data supplies executable argv. Archived environment
/// additions are raw executable intent and need their own owner decision.
pub(super) fn terminal(
    s: &ServerState,
    resource: &phux_core::ResourceKey,
    expected: phux_core::Revision,
) -> Result<Option<LaunchSpec>, Refusal> {
    let Some(record) = s.org().resource(resource) else {
        return Ok(None);
    };
    if record.revision != expected {
        return Ok(None);
    }
    if record.activation.as_ref().is_some_and(|activation| {
        activation.state.is_current() || activation.state == phux_core::ExecState::Lost
    }) {
        return Ok(None);
    }
    let Some(LaunchSpec::Terminal(launch)) = record.launch.as_ref() else {
        return Ok(None);
    };
    terminal_recipe(s, launch).map(Some)
}

fn terminal_recipe(
    s: &ServerState,
    launch: &phux_core::TerminalLaunch,
) -> Result<LaunchSpec, Refusal> {
    let deny = || {
        Refusal::InvalidArgument { field: "archived_recipe", reason:
        "no current trusted host template admits this recipe; inspect and explicitly approve its exact content on the local owner socket".into() }
    };
    let mut admitted = launch.clone();
    if let Some(bytes) = &launch.restore {
        let provenance: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| deny())?;
        let integration = provenance
            .get("integration_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(deny)?;
        let plugin = provenance
            .get("plugin_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(deny)?;
        let native_id = provenance
            .get("native_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(deny)?;
        let cwd = launch.relaunch_cwd().ok_or_else(deny)?;
        let resolved = phux_plugin::resolve_launch(
            &phux_config::loader::config_path(),
            integration,
            &[],
            cwd,
            phux_plugin::LaunchMode::Terminal,
        )
        .map_err(|_| deny())?;
        if resolved.plugin_id != plugin {
            return Err(deny());
        }
        admitted.argv = resolved.resume_argv(native_id).map_err(|_| deny())?;
        admitted.cwd = Some(resolved.cwd);
        let expected_env = vec![(
            resolved
                .session_identity
                .as_ref()
                .ok_or_else(deny)?
                .native_env
                .clone(),
            native_id.to_owned(),
        )];
        if !launch.env.is_empty() && launch.env != expected_env {
            return Err(deny());
        }
        admitted.env = expected_env;
        admitted.last_cwd = None;
        admitted.resume = None;
    } else if let Some(resume) = launch.resume.as_ref() {
        if !launch.env.is_empty() {
            return Err(deny());
        }
        if !resume.is_valid() {
            return Err(deny());
        }
        admitted.argv = s
            .config()
            .restore
            .resume_argv(&resume.app, &resume.token)
            .ok_or_else(deny)?;
        admitted.resume = None;
    } else {
        let mut shell = phux_terminal_api::launch::default_shell_command(
            s.config().shell.as_str(),
            s.config().login_shell,
        );
        phux_terminal_api::launch::apply_term(&mut shell, s.config().term.as_str());
        let current_argv = shell.get_argv();
        let same_shell = launch.argv.is_empty()
            || (launch.argv.len() == current_argv.len()
                && launch
                    .argv
                    .iter()
                    .zip(current_argv)
                    .all(|(saved, current)| std::ffi::OsStr::new(saved) == current.as_os_str()));
        let current_env = launch
            .env
            .iter()
            .all(|(key, value)| shell.get_env(key) == Some(std::ffi::OsStr::new(value)));
        if !same_shell || !current_env {
            return Err(deny());
        }
        // The current host's shell template owns these defaults. Retain the
        // semantic template, not its archived executable or injected environment.
        admitted.argv.clear();
        admitted.env.clear();
    }
    Ok(LaunchSpec::Terminal(admitted))
}

#[cfg(test)]
mod tests;
