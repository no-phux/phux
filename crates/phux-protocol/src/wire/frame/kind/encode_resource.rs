//! Payload writers for the L1 resource lifecycle and command/event frames.

use crate::ids::{GroupId, ResourceId, SatelliteHost};
use crate::wire::encode::Encoder;
use crate::wire::field;
use crate::wire::frame::{
    AgentEvent, CloseReason, Command, CommandResult, MoveResult, SpawnResource, SpawnResult,
    encode_agent_event, encode_command, encode_command_result, encode_env, encode_move_result,
    encode_server_instance, encode_spawn_result, encode_string_list,
};

use super::{FrameKind, id_field, u16_field, u32_field};

impl FrameKind {
    /// Write the request identity that opens the `SPAWN_RESOURCE` payload.
    pub(super) fn encode_spawn_terminal_request(
        enc: &mut Encoder<'_>,
        request_id: u32,
        group: GroupId,
    ) {
        u32_field(enc, field::spawn_terminal::REQUEST_ID, request_id);
        u32_field(enc, field::spawn_terminal::GROUP, group.get());
    }

    /// Write the process shape `SPAWN_RESOURCE` asks the server to launch.
    ///
    /// Optional command/cwd/env: absent field = None. An empty list
    /// (`Some(vec![])`) stays distinct: a present field with a zero count.
    pub(super) fn encode_spawn_terminal_process(
        enc: &mut Encoder<'_>,
        command: Option<&[String]>,
        cwd: Option<&str>,
        env_vars: Option<&[(String, String)]>,
        term: Option<&str>,
    ) {
        if let Some(cmd) = command {
            enc.write_field_with(field::spawn_terminal::COMMAND, |e| {
                encode_string_list(cmd, e);
            });
        }
        if let Some(c) = cwd {
            enc.write_field(field::spawn_terminal::CWD, c.as_bytes());
        }
        if let Some(vars) = env_vars {
            enc.write_field_with(field::spawn_terminal::ENV, |e| encode_env(vars, e));
        }
        if let Some(t) = term {
            enc.write_field(field::spawn_terminal::TERM, t.as_bytes());
        }
    }

    /// Write where the spawned terminal lands: host, owner, session, size.
    pub(super) fn encode_spawn_terminal_placement(
        enc: &mut Encoder<'_>,
        satellite: Option<&SatelliteHost>,
        owner_terminal: Option<&ResourceId>,
        agent_session: Option<&[u8]>,
        initial_size: Option<(u16, u16)>,
    ) {
        if let Some(host) = satellite {
            enc.write_field(field::spawn_terminal::SATELLITE, host.as_str().as_bytes());
        }
        if let Some(owner) = owner_terminal {
            id_field(enc, field::spawn_terminal::OWNER_TERMINAL, owner);
        }
        if let Some(value) = agent_session {
            enc.write_field(field::spawn_terminal::AGENT_SESSION, value);
        }
        if let Some((cols, rows)) = initial_size {
            enc.write_field_with(field::spawn_terminal::INITIAL_SIZE, |e| {
                e.write_u16_be(cols);
                e.write_u16_be(rows);
            });
        }
    }

    /// Write what kind of resource the spawn creates and, for a child kind,
    /// its binding and facet (fields 11-14).
    ///
    /// `kind` is written only when it is not the Terminal default, so a
    /// Terminal spawn's byte image is identical to one encoded before the
    /// field existed; the other three are absent-is-`None` optionals.
    pub(super) fn encode_spawn_terminal_resource(enc: &mut Encoder<'_>, resource: &SpawnResource) {
        if !resource.kind.is_terminal() {
            enc.write_field(field::spawn_terminal::KIND, &[resource.kind.as_wire()]);
        }
        if let Some(parent) = &resource.parent {
            id_field(enc, field::spawn_terminal::PARENT, parent);
        }
        if let Some(provider) = &resource.provider {
            enc.write_field(field::spawn_terminal::PROVIDER, provider.as_bytes());
        }
        if let Some(native_id) = &resource.native_id {
            enc.write_field(field::spawn_terminal::NATIVE_ID, native_id.as_bytes());
        }
        if resource.bind_instance {
            enc.write_field(field::spawn_terminal::BIND_INSTANCE, &[1]);
        }
        if let Some(secs) = resource.retain_secs {
            u32_field(enc, field::spawn_terminal::RETAIN_SECS, secs);
        }
        if let Some(key) = &resource.idempotency_key {
            enc.write_field(field::spawn_terminal::IDEMPOTENCY_KEY, key.as_bytes());
        }
    }

    /// Write the `RESOURCE_SPAWNED` payload. A bound result adds field 3 and
    /// a replayed one field 4, so a plain reply keeps the bytes it always
    /// had.
    pub(super) fn encode_terminal_spawned(
        enc: &mut Encoder<'_>,
        request_id: u32,
        result: &SpawnResult,
    ) {
        u32_field(enc, field::terminal_spawned::REQUEST_ID, request_id);
        enc.write_field_with(field::terminal_spawned::RESULT, |e| {
            encode_spawn_result(result, e);
        });
        if let Some(instance) = result.instance() {
            enc.write_field_with(field::terminal_spawned::INSTANCE, |e| {
                encode_server_instance(&instance, e);
            });
        }
        if result.is_replayed() {
            enc.write_field(field::terminal_spawned::REPLAYED, &[1]);
        }
    }

    /// Write the `MOVE_RESOURCE` payload.
    pub(super) fn encode_move_terminal(
        enc: &mut Encoder<'_>,
        request_id: u32,
        terminal: &ResourceId,
        owner_terminal: &ResourceId,
    ) {
        u32_field(enc, field::move_terminal::REQUEST_ID, request_id);
        id_field(enc, field::move_terminal::TERMINAL, terminal);
        id_field(enc, field::move_terminal::OWNER_TERMINAL, owner_terminal);
    }

    /// Write the `RESOURCE_MOVED` payload.
    pub(super) fn encode_terminal_moved(
        enc: &mut Encoder<'_>,
        request_id: u32,
        result: &MoveResult,
    ) {
        u32_field(enc, field::terminal_moved::REQUEST_ID, request_id);
        enc.write_field_with(field::terminal_moved::RESULT, |e| {
            encode_move_result(result, e);
        });
    }

    /// Write the `RESOURCE_CLOSED` payload.
    pub(super) fn encode_terminal_closed(
        enc: &mut Encoder<'_>,
        terminal_id: &ResourceId,
        exit_status: Option<i32>,
        reason: CloseReason,
    ) {
        id_field(enc, field::terminal_closed::TERMINAL_ID, terminal_id);
        // Optional exit status: absent field = signal / unknown.
        if let Some(status) = exit_status {
            u32_field(
                enc,
                field::terminal_closed::EXIT_STATUS,
                u32::from_be_bytes(status.to_be_bytes()),
            );
        }
        // Optional reason: absent field = unstated, so an unstated reason
        // leaves the body byte-identical to a pre-reason encoder's.
        if !reason.is_unknown() {
            enc.write_field(field::terminal_closed::REASON, &[reason.as_wire()]);
        }
    }

    /// Write the `RESIZE_TERMINAL` payload.
    pub(super) fn encode_terminal_resize(
        enc: &mut Encoder<'_>,
        terminal_id: &ResourceId,
        cols: u16,
        rows: u16,
        cell_px: Option<(u16, u16)>,
    ) {
        id_field(enc, field::terminal_resize::TERMINAL_ID, terminal_id);
        u16_field(enc, field::terminal_resize::COLS, cols);
        u16_field(enc, field::terminal_resize::ROWS, rows);
        if let Some((width, height)) = cell_px {
            u16_field(enc, field::terminal_resize::CELL_WIDTH_PX, width);
            u16_field(enc, field::terminal_resize::CELL_HEIGHT_PX, height);
        }
    }

    /// Write the `COMMAND` payload.
    pub(super) fn encode_command_frame(enc: &mut Encoder<'_>, request_id: u32, command: &Command) {
        u32_field(enc, field::command::REQUEST_ID, request_id);
        enc.write_field_with(field::command::COMMAND, |e| encode_command(command, e));
    }

    /// Write the `COMMAND_RESULT` payload.
    pub(super) fn encode_command_result_frame(
        enc: &mut Encoder<'_>,
        request_id: u32,
        result: &CommandResult,
    ) {
        u32_field(enc, field::command_result::REQUEST_ID, request_id);
        enc.write_field_with(field::command_result::RESULT, |e| {
            encode_command_result(result, e);
        });
    }

    /// Write the `SUBSCRIBE_EVENTS` payload.
    pub(super) fn encode_subscribe_events(enc: &mut Encoder<'_>, terminal: Option<&ResourceId>) {
        // Optional terminal scope: absent field = server-scoped None.
        if let Some(t) = terminal {
            id_field(enc, field::subscribe_events::TERMINAL, t);
        }
    }

    /// Write the `EVENT` payload's scope and event (fields 1-2).
    pub(super) fn encode_event(
        enc: &mut Encoder<'_>,
        terminal: Option<&ResourceId>,
        event: &AgentEvent,
    ) {
        if let Some(t) = terminal {
            id_field(enc, field::event::TERMINAL, t);
        }
        enc.write_field_with(field::event::EVENT, |e| encode_agent_event(event, e));
    }
}
