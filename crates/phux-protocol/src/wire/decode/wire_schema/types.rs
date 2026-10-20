//! Checking each declared value type against the codec's own decoders.

use super::{
    Decoder, decode_actor_ref, decode_agent_event, decode_attach_target, decode_bootstrap_codec,
    decode_bootstrap_profile, decode_client_capabilities, decode_command, decode_command_result,
    decode_env, decode_key_event, decode_mouse_event, decode_move_result, decode_paste_event,
    decode_scope, decode_server_capabilities, decode_session_snapshot, decode_spawn_result,
    decode_ssh_origin, decode_string_list, decode_terminal_id, decode_viewport_info,
};

/// Whether `value` decodes as `ty` and nothing is left over; `None` for a
/// type this checker does not know.
pub(super) fn type_matches(ty: &str, value: &[u8]) -> Option<bool> {
    let fixed = |width: usize| value.len() == width;
    Some(match ty {
        "u8" => fixed(1),
        "bool" => value == [0] || value == [1],
        "u16" => fixed(2),
        "u32" | "i32" | "GridSize" => fixed(4),
        "u64" => fixed(8),
        "bytes16" => fixed(16),
        "str" => std::str::from_utf8(value).is_ok(),
        "bytes" => true,
        "SshOrigin" => decode_ssh_origin(value).is_some(),
        "DirectoryEntryList" | "PathRowList" => whole(value, str_u8_list),
        _ => return positional_matches(ty, value),
    })
}

/// Named positional records, each checked by the codec's own decoder.
pub(super) fn positional_matches(ty: &str, value: &[u8]) -> Option<bool> {
    Some(match ty {
        "ResourceId" => whole(value, decode_terminal_id),
        "StringList" => whole(value, decode_string_list),
        "Env" => whole(value, decode_env),
        "Scope" => whole(value, decode_scope),
        "ActorRef" => whole(value, decode_actor_ref),
        "ViewportInfo" => whole(value, decode_viewport_info),
        "AttachTarget" => whole(value, decode_attach_target),
        "KeyEvent" => whole(value, decode_key_event),
        "MouseEvent" => whole(value, decode_mouse_event),
        "PasteEvent" => whole(value, decode_paste_event),
        "ClientCapabilities" => whole(value, decode_client_capabilities),
        "ServerCapabilities" => whole(value, decode_server_capabilities),
        "BootstrapProfile" => whole(value, decode_bootstrap_profile),
        "BootstrapCodec" => whole(value, decode_bootstrap_codec),
        "SessionSnapshot" => whole(value, decode_session_snapshot),
        "SpawnResult" => whole(value, decode_spawn_result),
        "MoveResult" => whole(value, decode_move_result),
        "Command" => whole(value, decode_command),
        "CommandResult" => whole(value, decode_command_result),
        "AgentEvent" => whole(value, decode_agent_event),
        _ => return None,
    })
}

/// Every type name [`type_matches`] knows.
pub(super) const KNOWN_TYPES: &[&str] = &[
    "u8",
    "bool",
    "u16",
    "u32",
    "i32",
    "GridSize",
    "u64",
    "bytes16",
    "str",
    "bytes",
    "SshOrigin",
    "DirectoryEntryList",
    "PathRowList",
    "ResourceId",
    "StringList",
    "Env",
    "Scope",
    "ActorRef",
    "ViewportInfo",
    "AttachTarget",
    "KeyEvent",
    "MouseEvent",
    "PasteEvent",
    "ClientCapabilities",
    "ServerCapabilities",
    "BootstrapProfile",
    "BootstrapCodec",
    "SessionSnapshot",
    "SpawnResult",
    "MoveResult",
    "Command",
    "CommandResult",
    "AgentEvent",
];

pub(super) fn whole<T>(
    value: &[u8],
    decode: impl FnOnce(&mut Decoder<'_>) -> Result<T, crate::wire::DecodeError>,
) -> bool {
    let mut dec = Decoder::new(value);
    decode(&mut dec).is_ok() && dec.remaining().is_empty()
}

/// `u32` count, then that many `(str, u8)` rows: directory entries and path
/// rows share this shape.
pub(super) fn str_u8_list(dec: &mut Decoder<'_>) -> Result<(), crate::wire::DecodeError> {
    for _ in 0..dec.read_u32_be()? {
        dec.read_str()?;
        dec.read_u8()?;
    }
    Ok(())
}
