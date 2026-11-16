//! Payload writers for the L1 resource lifecycle and command/event frames.

use crate::wire::encode::Encoder;
use crate::wire::field;
use crate::wire::frame::{
    SpawnResource, encode_agent_event, encode_command, encode_env, encode_server_instance,
    encode_spawn_result, encode_string_list,
};

use super::{FrameKind, encode_event_stamp, id_field};

impl FrameKind {
    /// Write a resource, command, or event frame's payload.
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per resource frame, each its field order"
    )]
    pub(super) fn encode_resource_payload(&self, enc: &mut Encoder<'_>) {
        match self {
            // Every optional is an absent field when `None`; an empty list
            // (`Some(vec![])`) stays a present field with a zero count.
            Self::SpawnResource {
                request_id,
                group,
                command,
                cwd,
                env,
                term,
                satellite,
                owner_terminal,
                agent_session,
                initial_size,
                resource,
            } => {
                use field::spawn_terminal as f;
                enc.write_u32_field(f::REQUEST_ID, *request_id);
                enc.write_u32_field(f::GROUP, group.get());
                if let Some(command) = command {
                    enc.write_field_with(f::COMMAND, |e| encode_string_list(command, e));
                }
                if let Some(cwd) = cwd {
                    enc.write_field(f::CWD, cwd.as_bytes());
                }
                if let Some(env) = env {
                    enc.write_field_with(f::ENV, |e| encode_env(env, e));
                }
                if let Some(term) = term {
                    enc.write_field(f::TERM, term.as_bytes());
                }
                if let Some(host) = satellite {
                    enc.write_field(f::SATELLITE, host.as_str().as_bytes());
                }
                if let Some(owner) = owner_terminal {
                    id_field(enc, f::OWNER_TERMINAL, owner);
                }
                if let Some(value) = agent_session {
                    enc.write_field(f::AGENT_SESSION, value);
                }
                if let Some((cols, rows)) = initial_size {
                    enc.write_field_with(f::INITIAL_SIZE, |e| {
                        e.write_u16_be(*cols);
                        e.write_u16_be(*rows);
                    });
                }
                if let Some(resource) = resource {
                    encode_spawn_resource(enc, resource);
                }
            }
            // A bound result adds field 3 and a replayed one field 4, so a
            // plain reply keeps the bytes it always had.
            Self::ResourceSpawned { request_id, result } => {
                enc.write_u32_field(field::terminal_spawned::REQUEST_ID, *request_id);
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
            Self::ResourceClosed {
                terminal_id,
                exit_status,
                reason,
                signal,
            } => {
                id_field(enc, field::terminal_closed::TERMINAL_ID, terminal_id);
                if let Some(status) = exit_status {
                    enc.write_i32_field(field::terminal_closed::EXIT_STATUS, *status);
                }
                // An unstated reason is an absent field: a pre-reason body.
                if !reason.is_unknown() {
                    enc.write_field(field::terminal_closed::REASON, &[reason.as_wire()]);
                }
                if let Some(signal) = signal {
                    enc.write_i32_field(field::terminal_closed::SIGNAL, *signal);
                }
            }
            Self::ResizeTerminal {
                terminal_id,
                cols,
                rows,
                cell_px,
            } => {
                id_field(enc, field::terminal_resize::TERMINAL_ID, terminal_id);
                enc.write_u16_field(field::terminal_resize::COLS, *cols);
                enc.write_u16_field(field::terminal_resize::ROWS, *rows);
                if let Some((width, height)) = cell_px {
                    enc.write_u16_field(field::terminal_resize::CELL_WIDTH_PX, *width);
                    enc.write_u16_field(field::terminal_resize::CELL_HEIGHT_PX, *height);
                }
            }
            Self::Command {
                request_id,
                command,
            } => {
                enc.write_u32_field(field::command::REQUEST_ID, *request_id);
                enc.write_field_with(field::command::COMMAND, |e| encode_command(command, e));
            }
            Self::Event {
                terminal,
                event,
                stamp,
            } => {
                if let Some(terminal) = terminal {
                    id_field(enc, field::event::TERMINAL, terminal);
                }
                enc.write_field_with(field::event::EVENT, |e| encode_agent_event(event, e));
                if let Some(stamp) = stamp {
                    encode_event_stamp(enc, stamp);
                }
            }
            _ => unreachable!("encode_payload routes only resource frames here"),
        }
    }
}

/// Write what kind of resource a spawn creates and, for a child kind, its
/// binding and facet (fields 11 on). `kind` is written only when it is not
/// the Terminal default, so a Terminal spawn keeps its pre-kind bytes.
fn encode_spawn_resource(enc: &mut Encoder<'_>, resource: &SpawnResource) {
    use field::spawn_terminal as f;
    if !resource.kind.is_terminal() {
        enc.write_field(f::KIND, &[resource.kind.as_wire()]);
    }
    if let Some(parent) = &resource.parent {
        id_field(enc, f::PARENT, parent);
    }
    if let Some(provider) = &resource.provider {
        enc.write_field(f::PROVIDER, provider.as_bytes());
    }
    if let Some(native_id) = &resource.native_id {
        enc.write_field(f::NATIVE_ID, native_id.as_bytes());
    }
    if resource.bind_instance {
        enc.write_field(f::BIND_INSTANCE, &[1]);
    }
    if let Some(secs) = resource.retain_secs {
        enc.write_u32_field(f::RETAIN_SECS, secs);
    }
    if let Some(key) = &resource.idempotency_key {
        enc.write_field(f::IDEMPOTENCY_KEY, key.as_bytes());
    }
    if let Some(control) = resource.control {
        enc.write_field(f::CONTROL, &[control.to_u8()]);
    }
}
