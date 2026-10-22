//! Encoding a `Command`: its tag, then its positional body.

use super::{
    COMMAND_TAG_ACQUIRE_INPUT, COMMAND_TAG_APPEND_RESOURCE_OUTPUT, COMMAND_TAG_APPLY_INPUT,
    COMMAND_TAG_ATTACH_RESOURCE, COMMAND_TAG_CLOSE_TAB_RESOURCES, COMMAND_TAG_DETACH_CLIENTS,
    COMMAND_TAG_DETACH_RESOURCE, COMMAND_TAG_GET_FILE, COMMAND_TAG_GET_PERF,
    COMMAND_TAG_GET_SCREEN, COMMAND_TAG_GET_STATE, COMMAND_TAG_GET_TERMINAL_STATE,
    COMMAND_TAG_KILL_RESOURCE, COMMAND_TAG_KILL_RESOURCE_IF, COMMAND_TAG_KILL_RESOURCES,
    COMMAND_TAG_OPEN_LISTENER, COMMAND_TAG_PERF_CONTROL, COMMAND_TAG_PUT_FILE,
    COMMAND_TAG_RELEASE_INPUT, COMMAND_TAG_REPORT_AGENT_STATE, COMMAND_TAG_REPORT_ASKED,
    COMMAND_TAG_ROUTE_INPUT, COMMAND_TAG_SHUTDOWN, COMMAND_TAG_SIGNAL_TERMINAL,
    COMMAND_TAG_SUBSCRIBE_RESOURCE_EVENTS, COMMAND_TAG_TRANSCRIBE, COMMAND_TAG_UPGRADE, ClientId,
    Command, Encoder, INPUT_EVENT_TAG_FOCUS, INPUT_EVENT_TAG_KEY, INPUT_EVENT_TAG_MOUSE,
    INPUT_EVENT_TAG_PASTE, IdempotencyKey, InputEvent, KillPrecondition, STATE_SCOPE_TAG_SERVER,
    StateScope, encode_asked_fields, encode_client_id, encode_focus_event, encode_key_event,
    encode_mouse_event, encode_optional_u32, encode_paste_event, encode_terminal_id,
};

#[allow(
    clippy::too_many_lines,
    reason = "one match arm per Command wire tag; the dispatch is a flat encode table, clearer whole than split"
)]
pub(in crate::wire) fn encode_command(command: &Command, enc: &mut Encoder<'_>) {
    match command {
        Command::DescribeApi => enc.write_u8(crate::wire::frame::COMMAND_TAG_DESCRIBE_API),
        Command::NativeAgent { request } => {
            enc.write_u8(crate::wire::frame::COMMAND_TAG_NATIVE_AGENT);
            enc.write_bytes(request.encoded());
        }
        Command::AttachResource {
            terminal_id,
            role_policy,
            agent_after_seq,
        } => {
            enc.write_u8(COMMAND_TAG_ATTACH_RESOURCE);
            encode_terminal_id(terminal_id, enc);
            enc.write_option(
                role_policy.map(crate::wire::frame::RolePolicy::to_u8),
                Encoder::write_u8,
            );
            enc.write_option(*agent_after_seq, Encoder::write_u64_be);
        }
        Command::DetachResource { terminal_id } => {
            enc.write_u8(COMMAND_TAG_DETACH_RESOURCE);
            encode_terminal_id(terminal_id, enc);
        }
        Command::KillResource {
            terminal_id,
            operation_id,
        } => {
            enc.write_u8(COMMAND_TAG_KILL_RESOURCE);
            encode_terminal_id(terminal_id, enc);
            encode_optional_key(operation_id.as_ref(), enc);
        }
        Command::KillResourceIf {
            terminal_id,
            precondition,
            operation_id,
        } => {
            enc.write_u8(COMMAND_TAG_KILL_RESOURCE_IF);
            encode_terminal_id(terminal_id, enc);
            encode_kill_precondition(precondition, enc);
            encode_optional_key(operation_id.as_ref(), enc);
        }
        Command::GetState { scope } => {
            enc.write_u8(COMMAND_TAG_GET_STATE);
            encode_state_scope(scope, enc);
        }
        Command::GetScreen {
            terminal_id,
            request_scrollback,
            cells,
            format,
        } => {
            enc.write_u8(COMMAND_TAG_GET_SCREEN);
            encode_terminal_id(terminal_id, enc);
            encode_optional_u32(*request_scrollback, enc);
            enc.write_u8(u8::from(*cells));
            enc.write_u8(*format);
        }
        Command::RouteInput { terminal_id, event } => {
            enc.write_u8(COMMAND_TAG_ROUTE_INPUT);
            encode_terminal_id(terminal_id, enc);
            encode_input_event(event, enc);
        }
        Command::ApplyInput {
            operation_id,
            terminal_id,
            events,
        } => {
            enc.write_u8(COMMAND_TAG_APPLY_INPUT);
            for byte in operation_id.as_bytes() {
                enc.write_u8(*byte);
            }
            encode_terminal_id(terminal_id, enc);
            enc.write_u16_be(u16::try_from(events.len()).unwrap_or(u16::MAX));
            for event in events {
                encode_input_event(event, enc);
            }
        }
        Command::KillResources { ids, operation_id } => {
            enc.write_u8(COMMAND_TAG_KILL_RESOURCES);
            encode_resource_ids(ids, enc);
            encode_optional_key(operation_id.as_ref(), enc);
        }
        Command::CloseTabResources { ids } => {
            enc.write_u8(COMMAND_TAG_CLOSE_TAB_RESOURCES);
            encode_resource_ids(ids, enc);
        }
        Command::DetachClients { session } => {
            enc.write_u8(COMMAND_TAG_DETACH_CLIENTS);
            enc.write_option(session.as_deref(), Encoder::write_str);
        }
        Command::GetTerminalState {
            terminal_id,
            include_scrollback,
            max_scrollback_lines,
        } => {
            enc.write_u8(COMMAND_TAG_GET_TERMINAL_STATE);
            encode_terminal_id(terminal_id, enc);
            enc.write_u8(u8::from(*include_scrollback));
            enc.write_u16_be(*max_scrollback_lines);
        }
        Command::SubscribeResourceEvents {
            terminal_id,
            event_types,
        } => {
            enc.write_u8(COMMAND_TAG_SUBSCRIBE_RESOURCE_EVENTS);
            encode_terminal_id(terminal_id, enc);
            enc.write_u16_be(u16::try_from(event_types.len()).unwrap_or(0));
            for et in event_types {
                enc.write_u8(et.to_u8());
            }
        }
        Command::Upgrade => {
            enc.write_u8(COMMAND_TAG_UPGRADE);
        }
        Command::Shutdown => {
            enc.write_u8(COMMAND_TAG_SHUTDOWN);
        }
        Command::OpenListener {
            transport,
            port_range,
            linger_secs,
        } => {
            enc.write_u8(COMMAND_TAG_OPEN_LISTENER);
            enc.write_u8(transport.to_u8());
            let (min, max) = port_range.unwrap_or((0, 0));
            enc.write_u16_be(min);
            enc.write_u16_be(max);
            enc.write_u32_be(*linger_secs);
        }
        Command::GetPerf { reset } => {
            enc.write_u8(COMMAND_TAG_GET_PERF);
            enc.write_u8(u8::from(*reset));
        }
        Command::PerfControl { op } => {
            enc.write_u8(COMMAND_TAG_PERF_CONTROL);
            enc.write_u8(op.to_u8());
        }
        Command::Transcribe {
            upload_id,
            terminal_id,
        } => {
            enc.write_u8(COMMAND_TAG_TRANSCRIBE);
            for byte in upload_id.as_bytes() {
                enc.write_u8(*byte);
            }
            encode_terminal_id(terminal_id, enc);
        }
        Command::AcquireInput {
            terminal_id,
            mode,
            ttl_ms,
        } => {
            enc.write_u8(COMMAND_TAG_ACQUIRE_INPUT);
            encode_terminal_id(terminal_id, enc);
            enc.write_u8(mode.to_u8());
            enc.write_u32_be(*ttl_ms);
        }
        Command::ReleaseInput { terminal_id } => {
            enc.write_u8(COMMAND_TAG_RELEASE_INPUT);
            encode_terminal_id(terminal_id, enc);
        }
        Command::SignalTerminal {
            terminal_id,
            signal,
            operation_id,
        } => {
            enc.write_u8(COMMAND_TAG_SIGNAL_TERMINAL);
            encode_terminal_id(terminal_id, enc);
            enc.write_u8(signal.to_u8());
            encode_optional_key(operation_id.as_ref(), enc);
        }
        Command::PutFile {
            upload_id,
            terminal_id,
            extension,
            offset,
            data,
            final_chunk,
            sha256,
        } => {
            enc.write_u8(COMMAND_TAG_PUT_FILE);
            for byte in upload_id.as_bytes() {
                enc.write_u8(*byte);
            }
            encode_terminal_id(terminal_id, enc);
            enc.write_str(extension);
            enc.write_u64_be(*offset);
            enc.write_bytes(data);
            enc.write_u8(u8::from(*final_chunk));
            enc.write_option(sha256.as_ref(), |e, digest| {
                for byte in digest {
                    e.write_u8(*byte);
                }
            });
        }
        Command::GetFile {
            host,
            path,
            offset,
            max_bytes,
        } => {
            enc.write_u8(COMMAND_TAG_GET_FILE);
            enc.write_option(host.as_deref(), Encoder::write_str);
            enc.write_str(path);
            enc.write_u64_be(*offset);
            enc.write_u32_be(*max_bytes);
        }
        Command::Checkout {
            terminal_id,
            request,
        } => {
            enc.write_u8(crate::wire::frame::COMMAND_TAG_CHECKOUT);
            encode_terminal_id(terminal_id, enc);
            enc.write_bytes(&crate::checkout::encode_request(request));
        }
        Command::ServiceHttpOpen { request } => super::service_http::encode_open(request, enc),
        Command::ServiceHttpRead {
            exchange,
            max_bytes,
        } => super::service_http::encode_read(*exchange, *max_bytes, enc),
        Command::ServiceHttpClose { exchange } => super::service_http::encode_close(*exchange, enc),
        Command::ReportAsked {
            terminal_id,
            id,
            question,
            suggestions,
            elapsed_seconds,
        } => {
            enc.write_u8(COMMAND_TAG_REPORT_ASKED);
            encode_terminal_id(terminal_id, enc);
            encode_asked_fields(id, question, suggestions, *elapsed_seconds, enc);
        }
        Command::ReportAgentState { terminal_id, state } => {
            enc.write_u8(COMMAND_TAG_REPORT_AGENT_STATE);
            encode_terminal_id(terminal_id, enc);
            enc.write_u8(state.to_u8());
        }
        Command::AppendResourceOutput { terminal_id, bytes } => {
            enc.write_u8(COMMAND_TAG_APPEND_RESOURCE_OUTPUT);
            encode_terminal_id(terminal_id, enc);
            enc.write_bytes(bytes);
        }
        org @ (Command::GetOrganization { .. }
        | Command::SessionCreate { .. }
        | Command::SessionRename { .. }
        | Command::SessionSetProject { .. }
        | Command::SessionSetLifetime { .. }
        | Command::SessionDelete { .. }
        | Command::SessionKill { .. }
        | Command::GetCreation { .. }
        | Command::ResourceCreate { .. }
        | Command::ResourceMove { .. }
        | Command::ResourceMoveMany { .. }
        | Command::ResourceRename { .. }
        | Command::ResourceTerminate { .. }
        | Command::ResourceRemove { .. }
        | Command::ResourceRelaunch { .. }
        | Command::InboxResolve { .. }
        | Command::OrganizationUndo { .. }
        | Command::ArrangementGet { .. }
        | Command::ArrangementReplace { .. }
        | Command::DelegateAgent { .. }
        | Command::RevokeDelegation { .. }) => {
            let written = crate::wire::frame::org::encode_org_command(org, enc);
            debug_assert!(written, "every field-tagged command has a tag");
        }
    }
}

/// Write an [`InputEvent`] as a record (`u32` length, then tag and body), so
/// an event can grow and a list of them stays walkable.
pub(super) fn encode_input_event(event: &InputEvent, enc: &mut Encoder<'_>) {
    enc.write_record(|enc| encode_input_event_body(event, enc));
}

pub(super) fn encode_input_event_body(event: &InputEvent, enc: &mut Encoder<'_>) {
    match event {
        InputEvent::Key(event) => {
            enc.write_u8(INPUT_EVENT_TAG_KEY);
            encode_key_event(event, enc);
        }
        InputEvent::Mouse(event) => {
            enc.write_u8(INPUT_EVENT_TAG_MOUSE);
            encode_mouse_event(event, enc);
        }
        InputEvent::Focus(event) => {
            enc.write_u8(INPUT_EVENT_TAG_FOCUS);
            enc.write_u8(encode_focus_event(*event));
        }
        InputEvent::Paste(event) => {
            enc.write_u8(INPUT_EVENT_TAG_PASTE);
            encode_paste_event(event, enc);
        }
    }
}

/// Write the optional `operation_id: bytes16` of a keyed supervisory
/// command (`docs/spec/L1.md` §5.1.1) behind a presence byte, so a field a
/// newer build appends after it stays unambiguous.
pub(super) fn encode_optional_key(key: Option<&IdempotencyKey>, enc: &mut Encoder<'_>) {
    enc.write_option(key, |e, key| {
        for byte in key.as_bytes() {
            e.write_u8(*byte);
        }
    });
}

/// Write a `KILL_RESOURCE_IF` precondition as a record (the trailing key
/// follows it): optional instance, then bits.
pub(super) fn encode_kill_precondition(precondition: &KillPrecondition, enc: &mut Encoder<'_>) {
    enc.write_record(|enc| {
        enc.write_option(precondition.instance.as_ref(), |e, instance| {
            crate::wire::frame::codec::encode_server_instance(instance, e);
        });
        enc.write_u8(precondition.conditions.bits());
    });
}

/// A `u16`-counted list of tagged `ResourceId`s.
pub(super) fn encode_resource_ids(ids: &[crate::ids::ResourceId], enc: &mut Encoder<'_>) {
    enc.write_u16_be(u16::try_from(ids.len()).unwrap_or(u16::MAX));
    for id in ids {
        encode_terminal_id(id, enc);
    }
}

pub(super) fn encode_optional_client_id(value: Option<ClientId>, enc: &mut Encoder<'_>) {
    enc.write_option(value, |e, id| encode_client_id(id, e));
}

pub(super) fn encode_state_scope(scope: &StateScope, enc: &mut Encoder<'_>) {
    match scope {
        StateScope::Server => enc.write_u8(STATE_SCOPE_TAG_SERVER),
    }
}
