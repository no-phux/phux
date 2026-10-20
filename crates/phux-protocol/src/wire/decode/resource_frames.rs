//! Resource spawn, move, close, resize, and command frame bodies.

use super::{
    CloseReason, DecodeError, Decoder, FrameKind, GroupId, ResourceId, SpawnResource,
    absorb_spawn_resource_field, bind_spawn_result, decode_command, decode_command_result,
    decode_env, decode_move_result, decode_spawn_result, decode_string_list, decode_terminal_id,
    field, req, utf8_value, validate_spawn_for_kind,
};

impl Decoder<'_> {
    pub(super) fn decode_spawn_terminal(&mut self) -> Result<FrameKind, DecodeError> {
        use field::spawn_terminal as f;
        let mut request_id = 0u32;
        let mut group = GroupId::new(0);
        let mut command: Option<Vec<String>> = None;
        let mut cwd: Option<String> = None;
        let mut env: Option<Vec<(String, String)>> = None;
        let mut term: Option<String> = None;
        let mut satellite: Option<crate::ids::SatelliteHost> = None;
        let mut owner_terminal: Option<crate::ids::ResourceId> = None;
        let mut agent_session: Option<Vec<u8>> = None;
        let mut initial_size: Option<(u16, u16)> = None;
        let mut resource = SpawnResource::default();
        while let Some((id, value)) = self.read_field()? {
            match id {
                f::REQUEST_ID => request_id = sub!(value, Decoder::read_u32_be),
                f::GROUP => group = GroupId::new(sub!(value, Decoder::read_u32_be)),
                f::COMMAND => command = Some(sub!(value, decode_string_list)),
                f::CWD => cwd = Some(utf8_value(value)?),
                f::ENV => env = Some(sub!(value, decode_env)),
                f::TERM => term = Some(utf8_value(value)?),
                f::SATELLITE => {
                    satellite = Some(crate::ids::SatelliteHost::new(
                        core::str::from_utf8(value).map_err(|_| DecodeError::InvalidUtf8)?,
                    ));
                }
                f::OWNER_TERMINAL => owner_terminal = Some(sub!(value, decode_terminal_id)),
                f::AGENT_SESSION => agent_session = Some(value.to_vec()),
                f::INITIAL_SIZE => {
                    initial_size = Some(sub!(value, |d: &mut Decoder<'_>| {
                        let cols = d.read_u16_be()?;
                        let rows = d.read_u16_be()?;
                        Ok((cols, rows))
                    }));
                }
                other => absorb_spawn_resource_field(&mut resource, other, value)?,
            }
        }
        // Default resource fields decode to `None`: canonical, pre-kind bytes.
        let resource = (!resource.is_default()).then(|| Box::new(resource));
        let frame = FrameKind::SpawnResource {
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
        };
        validate_spawn_for_kind(&frame)?;
        Ok(frame)
    }

    pub(super) fn decode_terminal_spawned(&mut self) -> Result<FrameKind, DecodeError> {
        use field::terminal_spawned as f;
        let mut request_id = 0u32;
        let mut result: Option<crate::wire::frame::SpawnResult> = None;
        let mut instance: Option<crate::ids::ServerInstance> = None;
        let mut replayed = false;
        while let Some((id, value)) = self.read_field()? {
            match id {
                f::REQUEST_ID => request_id = sub!(value, Decoder::read_u32_be),
                f::RESULT => result = Some(sub!(value, decode_spawn_result)),
                f::INSTANCE => {
                    instance = Some(sub!(value, crate::wire::frame::decode_server_instance));
                }
                f::REPLAYED => {
                    replayed = sub!(value, |d: &mut Decoder<'_>| decode_flag(d, "replayed"));
                }
                _ => {}
            }
        }
        let result = req(result)?;
        Ok(FrameKind::ResourceSpawned {
            request_id,
            result: bind_spawn_result(result, instance, replayed),
        })
    }

    pub(super) fn decode_move_terminal(&mut self) -> Result<FrameKind, DecodeError> {
        let mut request_id = 0u32;
        let mut terminal: Option<ResourceId> = None;
        let mut owner_terminal: Option<ResourceId> = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::move_terminal::REQUEST_ID => request_id = sub!(value, Decoder::read_u32_be),
                field::move_terminal::TERMINAL => terminal = Some(sub!(value, decode_terminal_id)),
                field::move_terminal::OWNER_TERMINAL => {
                    owner_terminal = Some(sub!(value, decode_terminal_id));
                }
                _ => {}
            }
        }
        Ok(FrameKind::MoveResource {
            request_id,
            terminal: req(terminal)?,
            owner_terminal: req(owner_terminal)?,
        })
    }

    pub(super) fn decode_terminal_moved(&mut self) -> Result<FrameKind, DecodeError> {
        let mut request_id = 0u32;
        let mut result: Option<crate::wire::frame::MoveResult> = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::terminal_moved::REQUEST_ID => request_id = sub!(value, Decoder::read_u32_be),
                field::terminal_moved::RESULT => result = Some(sub!(value, decode_move_result)),
                _ => {}
            }
        }
        Ok(FrameKind::ResourceMoved {
            request_id,
            result: req(result)?,
        })
    }

    pub(super) fn decode_terminal_closed(&mut self) -> Result<FrameKind, DecodeError> {
        use field::terminal_closed as f;
        let mut terminal_id: Option<ResourceId> = None;
        let mut exit_status: Option<i32> = None;
        let mut reason = CloseReason::Unknown;
        let mut signal: Option<i32> = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                f::TERMINAL_ID => terminal_id = Some(sub!(value, decode_terminal_id)),
                f::EXIT_STATUS => exit_status = Some(read_i32_value(value)?),
                f::REASON => reason = CloseReason::from_wire(sub!(value, Decoder::read_u8)),
                f::SIGNAL => signal = Some(read_i32_value(value)?),
                _ => {}
            }
        }
        Ok(FrameKind::ResourceClosed {
            terminal_id: req(terminal_id)?,
            exit_status,
            reason,
            signal,
        })
    }

    pub(super) fn decode_terminal_resize(&mut self) -> Result<FrameKind, DecodeError> {
        let mut terminal_id: Option<ResourceId> = None;
        let mut cols = 0u16;
        let mut rows = 0u16;
        let mut cell_w: Option<u16> = None;
        let mut cell_h: Option<u16> = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::terminal_resize::TERMINAL_ID => {
                    terminal_id = Some(sub!(value, decode_terminal_id));
                }
                field::terminal_resize::COLS => cols = sub!(value, Decoder::read_u16_be),
                field::terminal_resize::ROWS => rows = sub!(value, Decoder::read_u16_be),
                field::terminal_resize::CELL_WIDTH_PX => {
                    cell_w = Some(sub!(value, Decoder::read_u16_be));
                }
                field::terminal_resize::CELL_HEIGHT_PX => {
                    cell_h = Some(sub!(value, Decoder::read_u16_be));
                }
                _ => {}
            }
        }
        Ok(FrameKind::ResizeTerminal {
            terminal_id: req(terminal_id)?,
            cols,
            rows,
            // Both axes or neither (L1 §3.1): a half pair is no cell size.
            cell_px: cell_w.zip(cell_h),
        })
    }

    pub(super) fn decode_command(&mut self) -> Result<FrameKind, DecodeError> {
        let mut request_id = 0u32;
        let mut command: Option<crate::wire::frame::Command> = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::command::REQUEST_ID => request_id = sub!(value, Decoder::read_u32_be),
                field::command::COMMAND => command = Some(sub!(value, decode_command)),
                _ => {}
            }
        }
        Ok(FrameKind::Command {
            request_id,
            command: req(command)?,
        })
    }

    pub(super) fn decode_command_result(&mut self) -> Result<FrameKind, DecodeError> {
        let mut request_id = 0u32;
        let mut result: Option<crate::wire::frame::CommandResult> = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::command_result::REQUEST_ID => request_id = sub!(value, Decoder::read_u32_be),
                field::command_result::RESULT => result = Some(sub!(value, decode_command_result)),
                _ => {}
            }
        }
        Ok(FrameKind::CommandResult {
            request_id,
            result: req(result)?,
        })
    }
}

/// Read a field value that is an `i32` carried as its two's-complement `u32`.
pub(super) fn read_i32_value(value: &[u8]) -> Result<i32, DecodeError> {
    let bits = Decoder::new(value).read_u32_be()?;
    Ok(i32::from_be_bytes(bits.to_be_bytes()))
}

/// Read a one-byte flag: `0` or `1`; anything else is malformed.
pub(super) fn decode_flag(dec: &mut Decoder<'_>, field: &'static str) -> Result<bool, DecodeError> {
    match dec.read_u8()? {
        0 => Ok(false),
        1 => Ok(true),
        other => Err(DecodeError::UnknownEnumValue {
            field,
            value: u32::from(other),
        }),
    }
}
