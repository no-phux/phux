use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{BootstrapCapabilities, ServerCapabilities, select_bootstrap_profile};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, ErrorCode, FrameKind, Scope,
};

#[cfg(doc)]
use super::ScriptedServer;
use super::{OrgFixture, ScriptSpec};

/// The frames the reference server emits in response to one client frame,
/// **in the exact order it emits them**.
///
/// The only place test code decides what precedes an ack; a new arm cites
/// the server handler that justifies its ordering.
///
/// # Panics
///
/// On `ATTACH_RESOURCE` when the spec declared no priming snapshot — see
/// [`ScriptedServer::run`].
pub(super) fn reference_reply(frame: &FrameKind, spec: &mut ScriptSpec) -> Vec<FrameKind> {
    match frame {
        FrameKind::Hello { client_caps, .. } => {
            if let Some((code, message)) = spec.hello_error.clone() {
                return vec![FrameKind::Error {
                    request_id: None,
                    code,
                    message,
                }];
            }
            let (selected_profile, bootstrap_limits) =
                select_bootstrap_profile(client_caps, &BootstrapCapabilities::new())
                    .expect("default scripted server and client share a bootstrap profile");
            vec![FrameKind::HelloOk {
                protocol_major: PROTOCOL_VERSION.major,
                protocol_minor: PROTOCOL_VERSION.minor,
                protocol_patch: PROTOCOL_VERSION.patch,
                server_caps: ServerCapabilities::new()
                    .with_features(spec.server_features)
                    .with_features_ext(spec.server_features_ext),
                server_id: spec.server_id.clone(),
                selected_profile,
                bootstrap_limits,
            }]
        }
        FrameKind::Command {
            request_id,
            command,
        } => command_reply(*request_id, command, spec),
        // `handle_subscribe_events` / `handle_subscribe_metadata` register
        // and reply with nothing; an ack would teach a client to wait forever.
        #[allow(
            clippy::match_same_arms,
            reason = "documents an ordering fact, not a fallthrough"
        )]
        FrameKind::SubscribeEvents { .. } | FrameKind::SubscribeMetadata { .. } => Vec::new(),
        FrameKind::GetMetadata {
            request_id,
            scope,
            key,
        } => {
            let mut out = std::mem::take(&mut spec.pre_ack);
            out.push(metadata_reply(*request_id, scope, key, spec));
            out
        }
        FrameKind::SetMetadata {
            request_id,
            scope,
            key,
            value,
        } => set_metadata_reply(*request_id, scope, key, value, spec),
        FrameKind::DeleteMetadata { scope, key, .. } => {
            spec.metadata_store
                .retain(|(stored_scope, stored_key, _)| stored_scope != scope || stored_key != key);
            Vec::new()
        }
        // `handle_spawn_terminal`: `RESOURCE_SPAWNED` after anything queued,
        // or a hub-relayed correlated `ERROR`.
        FrameKind::SpawnResource { request_id, .. } => {
            let mut out = std::mem::take(&mut spec.pre_ack);
            if let Some((code, message)) = spec.spawn_error.clone() {
                out.push(FrameKind::Error {
                    request_id: Some(*request_id),
                    code,
                    message,
                });
                return out;
            }
            let result = spec.spawn.clone().expect(
                "a scripted server whose client sends SPAWN_RESOURCE must declare an \
                 outcome: handle_spawn_terminal always answers. Call \
                 ScriptSpec::spawn_result or ScriptSpec::refuse_spawn.",
            );
            out.push(FrameKind::ResourceSpawned {
                request_id: *request_id,
                result,
            });
            out
        }
        FrameKind::MoveResource { request_id, .. } => vec![FrameKind::ResourceMoved {
            request_id: *request_id,
            result: spec.move_result.clone().expect(
                "a scripted server whose client sends MOVE_RESOURCE must declare an outcome",
            ),
        }],
        // Answered by the real server but not modelled yet: fail loudly
        // rather than wedge the client with silence.
        FrameKind::Attach { .. } | FrameKind::ListMetadata { .. } => {
            panic!(
                "the scripted server has no reference ordering for {frame:?} yet; add an \
                 arm to `reference_reply` citing the server handler"
            )
        }
        // Everything else is fire-and-forget on this wire.
        _ => Vec::new(),
    }
}

/// `SET_METADATA` has no reply (`handle_set_metadata`); the write is stored
/// unless pinned by [`ScriptSpec::drop_metadata_writes`], so a confirming
/// read sees it.
fn set_metadata_reply(
    request_id: u32,
    scope: &Scope,
    key: &str,
    value: &[u8],
    spec: &mut ScriptSpec,
) -> Vec<FrameKind> {
    if let Some((code, message)) = spec.metadata_error.clone() {
        return vec![FrameKind::Error {
            request_id: Some(request_id),
            code,
            message,
        }];
    }
    if spec
        .dropped_metadata_writes
        .contains(&(scope.clone(), key.to_owned()))
    {
        return Vec::new();
    }
    spec.metadata_store
        .retain(|(s, k, _)| !(s == scope && k == key));
    spec.metadata_store
        .push((scope.clone(), key.to_owned(), value.to_vec()));
    Vec::new()
}

/// Fault injection changes the payload, never command correlation or ordering.
fn injected_mutation_reply(command: &Command, spec: &ScriptSpec) -> Option<CommandResult> {
    if !command.is_organization() || command.idempotency_key().is_none() {
        return None;
    }
    spec.mutation_reply.clone()
}

/// The reply sequence for one `COMMAND`, pre-ack pushes first.
fn command_reply(request_id: u32, command: &Command, spec: &mut ScriptSpec) -> Vec<FrameKind> {
    // Every command ack is preceded by whatever was queued.
    let mut out = std::mem::take(&mut spec.pre_ack);
    if let Some(result) = injected_mutation_reply(command, spec) {
        out.push(FrameKind::CommandResult { request_id, result });
        return out;
    }
    match command {
        Command::AttachResource { .. } => {
            assert!(
                !spec.priming.is_empty(),
                "a scripted server whose client sends ATTACH_RESOURCE must declare \
                 priming bootstrap frames. Call ScriptSpec::priming_snapshot."
            );
            out.extend(spec.priming.clone());
            out.push(FrameKind::CommandResult {
                request_id,
                result: CommandResult::Ok,
            });
        }
        // The organization commands: `COMMITTED`, `ACCEPTED`, a value, or
        // `REFUSED`, from the scripted organization.
        command if command.is_organization() && spec.organization.is_some() => {
            let result = spec
                .organization
                .as_mut()
                .map_or(CommandResult::Ok, |org| org.answer(command));
            out.push(FrameKind::CommandResult { request_id, result });
        }
        Command::GetState { .. } => {
            let snapshot = spec
                .states
                .pop_front()
                .or_else(|| spec.state.clone())
                .or_else(|| spec.organization.as_ref().map(OrgFixture::snapshot));
            let result = match (&spec.state_error, snapshot) {
                (Some((code, message)), _) => CommandResult::Error {
                    code: *code,
                    message: message.clone(),
                },
                (None, snapshot) => snapshot.map_or(CommandResult::Ok, |snapshot| {
                    CommandResult::OkWith(CommandValue::State(snapshot))
                }),
            };
            out.push(FrameKind::CommandResult { request_id, result });
            // An event that happened after the snapshot was cut: see
            // `ScriptSpec::push_after_state`.
            if let Some(batch) = spec.after_state.pop_front() {
                out.extend(batch);
            }
        }
        // `handle_get_terminal_state` answers `OK_WITH(JSON(..))`.
        Command::GetTerminalState { .. } if spec.terminal_state.is_some() => {
            let json = spec.terminal_state.clone().unwrap_or_default();
            out.push(FrameKind::CommandResult {
                request_id,
                result: CommandResult::OkWith(CommandValue::Json(json)),
            });
        }
        // `handle_detach_clients` always answers `OkWith(Json(count))`.
        Command::DetachClients { .. } => {
            let count = spec.detach_result.unwrap_or(0);
            out.push(FrameKind::CommandResult {
                request_id,
                result: CommandResult::OkWith(CommandValue::Json(count.to_string())),
            });
        }
        // `APPEND_RESOURCE_OUTPUT`: the test supplies the result.
        Command::AppendResourceOutput { .. } => {
            let result = spec.append_result.clone().unwrap_or(CommandResult::Ok);
            out.push(FrameKind::CommandResult { request_id, result });
        }
        // `handle_upgrade` answers a failed preparation with an
        // `INTERNAL_ERROR` result; success is the default `Ok` ack.
        Command::Upgrade if spec.upgrade_error.is_some() => {
            out.push(FrameKind::CommandResult {
                request_id,
                result: CommandResult::Error {
                    code: ErrorCode::InternalError,
                    message: spec.upgrade_error.clone().unwrap_or_default(),
                },
            });
        }
        // A wedged server, on purpose: see `ScriptSpec::wedge_screen_reads`.
        Command::GetScreen { .. } if spec.wedge_screen_reads => {}
        // `handle_get_screen` answers `OK_WITH(JSON(ScreenState))`.
        Command::GetScreen { .. } if spec.screen.is_some() => {
            let json = spec.screen.clone().unwrap_or_default();
            out.push(FrameKind::CommandResult {
                request_id,
                result: CommandResult::OkWith(CommandValue::Json(json)),
            });
        }
        // A wedged server, on purpose: see `ScriptSpec::wedge_input_after`.
        Command::RouteInput { .. } if spec.input_acks_left == Some(0) => {}
        Command::RouteInput { .. } => {
            if let Some(left) = spec.input_acks_left.as_mut() {
                *left -= 1;
            }
            out.push(FrameKind::CommandResult {
                request_id,
                result: CommandResult::Ok,
            });
        }
        _ => out.push(FrameKind::CommandResult {
            request_id,
            result: CommandResult::Ok,
        }),
    }
    out
}

/// The reply to one `GET_METADATA`: a refusal, this session's own stored
/// write, or the spec's canned answer — in that precedence.
fn metadata_reply(request_id: u32, scope: &Scope, key: &str, spec: &mut ScriptSpec) -> FrameKind {
    if let Some((code, message)) = spec.metadata_error.clone() {
        return FrameKind::Error {
            request_id: Some(request_id),
            code,
            message,
        };
    }
    let stored = spec
        .metadata_store
        .iter()
        .find(|(s, k, _)| s == scope && k == key)
        .map(|(_, _, value)| value.clone());
    let value = stored.or_else(|| {
        spec.metadata
            .as_mut()
            .and_then(|responder| responder(scope, key))
    });
    FrameKind::MetadataValue { request_id, value }
}
