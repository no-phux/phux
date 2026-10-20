//! Proptest strategies for input, metadata, and lifecycle frames and their leaf values.

use proptest::prelude::*;

use super::{
    Command, CommandResult, DetachReason, ErrorCode, FocusEvent, FrameKind, GroupId, KeyAction,
    KeyEvent, ModSet, MouseAction, MouseButton, MouseEvent, PasteEvent, PasteTrust, PhysicalKey,
    ResourceId, arb_actor, arb_agent_event, arb_close_reason, arb_directory_listing_result,
    arb_env_pair, arb_event_stamp, arb_metadata_value, arb_move_result, arb_scope,
    arb_spawn_result,
};

pub(super) fn arb_frame_kind_input() -> impl Strategy<Value = FrameKind> {
    prop_oneof![
        (arb_terminal_id(), arb_key_event())
            .prop_map(|(terminal_id, event)| FrameKind::InputKey { terminal_id, event }),
        (arb_terminal_id(), arb_mouse_event())
            .prop_map(|(terminal_id, event)| FrameKind::InputMouse { terminal_id, event }),
        (arb_terminal_id(), arb_focus_event())
            .prop_map(|(terminal_id, event)| FrameKind::InputFocus { terminal_id, event }),
        (arb_terminal_id(), arb_paste_event())
            .prop_map(|(terminal_id, event)| FrameKind::InputPaste { terminal_id, event }),
    ]
}

pub(super) fn arb_frame_kind_metadata() -> impl Strategy<Value = FrameKind> {
    prop_oneof![
        (any::<u32>(), arb_scope(), ".{0,64}").prop_map(|(request_id, scope, key)| {
            FrameKind::GetMetadata {
                request_id,
                scope,
                key,
            }
        }),
        (any::<u32>(), arb_scope(), ".{0,64}", arb_metadata_value(),).prop_map(
            |(request_id, scope, key, value)| FrameKind::SetMetadata {
                request_id,
                scope,
                key,
                value,
            }
        ),
        (any::<u32>(), arb_scope(), ".{0,64}").prop_map(|(request_id, scope, key)| {
            FrameKind::DeleteMetadata {
                request_id,
                scope,
                key,
            }
        }),
        (any::<u32>(), arb_scope())
            .prop_map(|(request_id, scope)| FrameKind::ListMetadata { request_id, scope }),
        (arb_scope(), ".{0,64}")
            .prop_map(|(scope, key)| FrameKind::SubscribeMetadata { scope, key }),
        (
            arb_scope(),
            ".{0,64}",
            proptest::option::of(arb_metadata_value()),
            proptest::option::of(arb_actor()),
        )
            .prop_map(|(scope, key, value, actor)| FrameKind::MetadataChanged {
                scope,
                key,
                value,
                actor,
            }),
        (any::<u32>(), proptest::option::of(arb_metadata_value()))
            .prop_map(|(request_id, value)| FrameKind::MetadataValue { request_id, value },),
        (any::<u32>(), proptest::collection::vec(".{0,32}", 0..8),)
            .prop_map(|(request_id, keys)| FrameKind::MetadataKeys { request_id, keys }),
        (
            any::<u32>(),
            ".{0,64}",
            proptest::option::of("[a-z0-9.-]{0,32}"),
        )
            .prop_map(|(request_id, path, host)| FrameKind::ListDirectory {
                request_id,
                path,
                host: host.map(phux_protocol::ids::SatelliteHost::new),
            }),
        (any::<u32>(), arb_directory_listing_result()).prop_map(|(request_id, result)| {
            FrameKind::DirectoryListing { request_id, result }
        }),
    ]
}

pub(super) fn arb_plain_spawn_resource() -> impl Strategy<Value = FrameKind> {
    (
        any::<u32>(),
        any::<u32>(),
        proptest::option::of(proptest::collection::vec(".{0,16}", 0..4)),
        proptest::option::of(".{0,32}"),
        proptest::option::of(proptest::collection::vec(arb_env_pair(), 0..4)),
        proptest::option::of(".{0,16}"),
        proptest::option::of(".{0,16}"),
        proptest::option::of(any::<u32>()),
        proptest::option::of(proptest::collection::vec(any::<u8>(), 0..64)),
        proptest::option::of((any::<u16>(), any::<u16>())),
    )
        .prop_map(
            |(
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
            )| {
                FrameKind::SpawnResource {
                    request_id,
                    group: GroupId::new(group),
                    command,
                    cwd,
                    env,
                    term,
                    satellite: satellite.map(phux_protocol::ids::SatelliteHost::new),
                    owner_terminal: owner_terminal.map(ResourceId::local),
                    agent_session,
                    initial_size,
                    resource: None,
                }
            },
        )
}

pub(super) fn arb_frame_kind_lifecycle() -> impl Strategy<Value = FrameKind> {
    prop_oneof![
        arb_plain_spawn_resource(),
        (any::<u32>(), arb_spawn_result())
            .prop_map(|(request_id, result)| { FrameKind::ResourceSpawned { request_id, result } }),
        (any::<u32>(), arb_terminal_id(), arb_terminal_id()).prop_map(
            |(request_id, terminal, owner_terminal)| FrameKind::MoveResource {
                request_id,
                terminal,
                owner_terminal,
            },
        ),
        (any::<u32>(), arb_move_result())
            .prop_map(|(request_id, result)| { FrameKind::ResourceMoved { request_id, result } }),
        (
            arb_terminal_id(),
            proptest::option::of(any::<i32>()),
            arb_close_reason(),
            proptest::option::of(any::<i32>()),
        )
            .prop_map(|(terminal_id, exit_status, reason, signal)| {
                FrameKind::ResourceClosed {
                    terminal_id,
                    exit_status,
                    reason,
                    signal,
                }
            }),
        (
            arb_terminal_id(),
            any::<u16>(),
            any::<u16>(),
            proptest::option::of((any::<u16>(), any::<u16>())),
        )
            .prop_map(
                |(terminal_id, cols, rows, cell_px)| FrameKind::ResizeTerminal {
                    terminal_id,
                    cols,
                    rows,
                    cell_px,
                }
            ),
        (any::<u32>(), arb_terminal_id()).prop_map(|(request_id, terminal_id)| {
            FrameKind::Command {
                request_id,
                command: Command::KillResource {
                    terminal_id,
                    operation_id: None,
                },
            }
        }),
        any::<u32>().prop_map(|request_id| FrameKind::CommandResult {
            request_id,
            result: CommandResult::Ok,
        }),
        (any::<u32>(), ".{0,48}").prop_map(|(request_id, message)| FrameKind::CommandResult {
            request_id,
            result: CommandResult::Error {
                code: ErrorCode::InvalidCommand,
                message,
            },
        }),
        (
            proptest::option::of(arb_terminal_id()),
            proptest::option::of(any::<u64>()),
        )
            .prop_map(|(terminal, after_seq)| FrameKind::SubscribeEvents {
                terminal,
                after_seq
            }),
        (
            proptest::option::of(arb_terminal_id()),
            arb_agent_event(),
            proptest::option::of(arb_event_stamp()),
        )
            .prop_map(|(terminal, event, stamp)| FrameKind::Event {
                terminal,
                event,
                stamp: stamp.map(Box::new),
            }),
    ]
}

/// Strategy over every `DETACHED` reason shape, including the absent one a
/// server predating `0.7.0-draft.7` encodes.
pub(super) fn arb_detach_reason() -> impl Strategy<Value = Option<DetachReason>> {
    prop_oneof![
        Just(None),
        Just(Some(DetachReason::Requested)),
        Just(Some(DetachReason::ServerShutdown)),
        Just(Some(DetachReason::SessionKilled)),
        Just(Some(DetachReason::Replaced)),
        Just(Some(DetachReason::ProtocolError)),
        Just(Some(DetachReason::AuthenticationFailed)),
        Just(Some(DetachReason::AuthorizationRevoked)),
        Just(Some(DetachReason::AuthorizationExpired)),
        Just(Some(DetachReason::InternalError)),
    ]
}

/// Strategy producing both `Local` and `Satellite` variants of [`ResourceId`].
/// v0.1 servers only emit `Local`, but v0.1 decoders MUST round-trip both
/// shapes (the dispatch layer is what rejects `Satellite` ids with
/// `UnsupportedSatelliteRoute`).
pub(super) fn arb_terminal_id() -> impl Strategy<Value = ResourceId> {
    prop_oneof![
        any::<u32>().prop_map(ResourceId::local),
        (".{0,32}", any::<u32>()).prop_map(|(host, id)| ResourceId::satellite(host, id)),
    ]
}

pub(super) fn arb_focus_event() -> impl Strategy<Value = FocusEvent> {
    prop_oneof![Just(FocusEvent::Gained), Just(FocusEvent::Lost)]
}

pub(super) fn arb_key_action() -> impl Strategy<Value = KeyAction> {
    prop_oneof![
        Just(KeyAction::Press),
        Just(KeyAction::Release),
        Just(KeyAction::Repeat),
    ]
}

pub(super) fn arb_physical_key() -> impl Strategy<Value = PhysicalKey> {
    prop_oneof![
        Just(PhysicalKey::Unidentified),
        Just(PhysicalKey::A),
        Just(PhysicalKey::Enter),
        Just(PhysicalKey::Escape),
        Just(PhysicalKey::ArrowUp),
        Just(PhysicalKey::F1),
        Just(PhysicalKey::Numpad7),
    ]
}

pub(super) fn arb_mod_set() -> impl Strategy<Value = ModSet> {
    any::<u16>().prop_map(|bits| ModSet::from_bits_truncate(bits & ModSet::all().bits()))
}

pub(super) fn arb_key_event() -> impl Strategy<Value = KeyEvent> {
    (
        arb_key_action(),
        arb_physical_key(),
        arb_mod_set(),
        arb_mod_set(),
        any::<bool>(),
        proptest::option::of(prop::string::string_regex("[a-zA-Z0-9 ]{0,8}").unwrap()),
        proptest::option::of(any::<u32>()),
    )
        .prop_map(
            |(action, key, mods, consumed_mods, composing, text, unshifted_codepoint)| KeyEvent {
                action,
                key,
                mods,
                consumed_mods,
                composing,
                text,
                unshifted_codepoint,
            },
        )
}

pub(super) fn arb_mouse_action() -> impl Strategy<Value = MouseAction> {
    prop_oneof![
        Just(MouseAction::Press),
        Just(MouseAction::Release),
        Just(MouseAction::Motion),
    ]
}

pub(super) fn arb_mouse_button() -> impl Strategy<Value = MouseButton> {
    prop_oneof![
        Just(MouseButton::Unknown),
        Just(MouseButton::Left),
        Just(MouseButton::Right),
        Just(MouseButton::Middle),
        Just(MouseButton::Four),
        Just(MouseButton::Eleven),
    ]
}

pub(super) fn arb_mouse_event() -> impl Strategy<Value = MouseEvent> {
    (
        arb_mouse_action(),
        arb_mouse_button(),
        arb_mod_set(),
        prop::num::f64::NORMAL | prop::num::f64::ZERO | prop::num::f64::SUBNORMAL,
        prop::num::f64::NORMAL | prop::num::f64::ZERO | prop::num::f64::SUBNORMAL,
    )
        .prop_map(|(action, button, mods, x, y)| MouseEvent {
            action,
            button,
            mods,
            x,
            y,
        })
}

/// One of every `ErrorCode` known to SPEC §14. The decoder must round-trip
/// every wire value defined by the spec.
pub(super) fn arb_error_code() -> impl Strategy<Value = ErrorCode> {
    prop_oneof![
        Just(ErrorCode::VersionIncompatible),
        Just(ErrorCode::UnknownMessageType),
        Just(ErrorCode::MalformedMessage),
        Just(ErrorCode::FrameTooLarge),
        Just(ErrorCode::NotAttached),
        Just(ErrorCode::AlreadyAttached),
        Just(ErrorCode::SessionNotFound),
        Just(ErrorCode::WindowNotFound),
        Just(ErrorCode::TerminalNotFound),
        Just(ErrorCode::ClientNotFound),
        Just(ErrorCode::UnsupportedSatelliteRoute),
        Just(ErrorCode::SatelliteUnreachable),
        Just(ErrorCode::InvalidCommand),
        Just(ErrorCode::PermissionDenied),
        Just(ErrorCode::ResourceExhausted),
        Just(ErrorCode::UnsafePaste),
        Just(ErrorCode::InputDeliveryUnknown),
        Just(ErrorCode::InputLeaseHeld),
        Just(ErrorCode::CanonicalLimitExceeded),
        Just(ErrorCode::InputNotWritten),
        Just(ErrorCode::WrongResourceKind),
        Just(ErrorCode::NotProducer),
        Just(ErrorCode::RecordInvalid),
        Just(ErrorCode::Overflow),
        Just(ErrorCode::InternalError),
    ]
}

pub(super) fn arb_paste_event() -> impl Strategy<Value = PasteEvent> {
    (
        prop_oneof![Just(PasteTrust::Trusted), Just(PasteTrust::Untrusted)],
        proptest::collection::vec(any::<u8>(), 0..64),
    )
        .prop_map(|(trust, data)| PasteEvent { trust, data })
}

/// VT byte stream, capped at 4 KiB for test speed. Empty payloads are
/// legal — `RESOURCE_OUTPUT` carries whatever the PTY produced, including
/// zero bytes (which the rate-limiter just won't emit, but the codec
/// must round-trip).
pub(super) fn arb_vt_bytes() -> impl Strategy<Value = Vec<u8>> {
    proptest::collection::vec(any::<u8>(), 0..4096)
}
