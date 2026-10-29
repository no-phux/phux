//! Detach, input, viewport, error, L3 metadata, and directory fixtures.

use phux_protocol::ids::{GroupId, ResourceId};
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{DetachReason, ErrorCode, FrameKind, Scope, ViewportInfo};

#[allow(clippy::too_many_lines)]
pub(super) fn fixtures() -> Vec<(&'static str, FrameKind)> {
    vec![
        // DETACH is a unit message; DETACHED carries two optional-absent
        // fields, so the reason-less shape must stay byte-identical to the
        // empty body every 0.7.0 peer already emits.
        ("snap_detach", FrameKind::Detach),
        (
            "snap_detached",
            FrameKind::Detached {
                reason: None,
                message: String::new(),
            },
        ),
        (
            "snap_detached_reason",
            FrameKind::Detached {
                reason: Some(DetachReason::ServerShutdown),
                message: "server is stopping".to_owned(),
            },
        ),
        // INPUT_*
        (
            "snap_input_key_letter_a_press",
            FrameKind::InputKey {
                terminal_id: ResourceId::local(0x0000_0007),
                event: KeyEvent {
                    action: KeyAction::Press,
                    key: PhysicalKey::A,
                    mods: ModSet::empty(),
                    consumed_mods: ModSet::empty(),
                    composing: false,
                    text: Some("a".to_owned()),
                    unshifted_codepoint: Some(u32::from('a')),
                },
            },
        ),
        (
            "snap_input_key_no_text",
            FrameKind::InputKey {
                terminal_id: ResourceId::local(0x0000_0001),
                event: KeyEvent {
                    action: KeyAction::Release,
                    key: PhysicalKey::Escape,
                    mods: ModSet::CTRL | ModSet::SHIFT,
                    consumed_mods: ModSet::empty(),
                    composing: false,
                    text: None,
                    unshifted_codepoint: None,
                },
            },
        ),
        (
            "snap_input_mouse_left_click",
            FrameKind::InputMouse {
                terminal_id: ResourceId::local(0x0000_0042),
                event: MouseEvent {
                    action: MouseAction::Press,
                    button: MouseButton::Left,
                    mods: ModSet::empty(),
                    x: 120.0,
                    y: 40.5,
                },
            },
        ),
        (
            "snap_input_focus_gained",
            FrameKind::InputFocus {
                terminal_id: ResourceId::local(0x0000_0003),
                event: FocusEvent::Gained,
            },
        ),
        (
            "snap_input_focus_lost",
            FrameKind::InputFocus {
                terminal_id: ResourceId::local(0x0000_0003),
                event: FocusEvent::Lost,
            },
        ),
        (
            "snap_input_paste_trusted_ascii",
            FrameKind::InputPaste {
                terminal_id: ResourceId::local(0x0000_0005),
                event: PasteEvent {
                    trust: PasteTrust::Trusted,
                    data: b"hello world".to_vec(),
                },
            },
        ),
        (
            "snap_bell",
            FrameKind::Bell {
                terminal_id: ResourceId::local(0x0000_00BE),
            },
        ),
        // VIEWPORT_RESIZE — cell-only and pixel-augmented viewports.
        (
            "snap_viewport_resize_cells_only",
            FrameKind::ViewportResize {
                viewport: ViewportInfo::new(120, 40),
            },
        ),
        (
            "snap_viewport_resize_with_pixels",
            FrameKind::ViewportResize {
                viewport: ViewportInfo::new(120, 40).with_pixels(Some(1920), Some(1080)),
            },
        ),
        // ERROR — server-emitted structured error frames; sibling refusal
        // paths share the wire shape. The internal fixture exercises the
        // u16::MAX (=65535) wire value to lock in the high end of the
        // ErrorCode encoding alongside SPEC §14's `INTERNAL_ERROR = 65535`.
        (
            "snap_error_session_not_found",
            FrameKind::Error {
                request_id: None,
                code: ErrorCode::SessionNotFound,
                message: "no such session: 'work'".to_owned(),
            },
        ),
        (
            "snap_error_with_request_id_invalid_command",
            FrameKind::Error {
                request_id: Some(0x0000_002A),
                code: ErrorCode::InvalidCommand,
                message: "missing field: terminal_id".to_owned(),
            },
        ),
        (
            "snap_error_internal_max_code",
            FrameKind::Error {
                request_id: None,
                code: ErrorCode::InternalError,
                message: String::new(),
            },
        ),
        // L3 metadata frames.
        (
            "snap_get_metadata_global",
            FrameKind::GetMetadata {
                request_id: 0x0000_0001,
                scope: Scope::Global,
                key: "phux.example/v1".to_owned(),
            },
        ),
        (
            "snap_get_metadata_group",
            FrameKind::GetMetadata {
                request_id: 0x0000_0007,
                scope: Scope::Group(GroupId::new(1)),
                key: "phux.tui.layout/v1".to_owned(),
            },
        ),
        (
            "snap_get_metadata_terminal",
            FrameKind::GetMetadata {
                request_id: 0x0000_0042,
                scope: Scope::Resource(ResourceId::local(0x0000_0009)),
                key: "phux.tui.title-override/v1".to_owned(),
            },
        ),
        (
            "snap_set_metadata_group_layout",
            FrameKind::SetMetadata {
                request_id: 0x0000_0010,
                scope: Scope::Group(GroupId::new(1)),
                key: "phux.tui.layout/v1".to_owned(),
                value: b"\xa2\x01\x01\x02\x82\x00\x01".to_vec(), // arbitrary CBOR-looking bytes
            },
        ),
        (
            "snap_delete_metadata_global",
            FrameKind::DeleteMetadata {
                request_id: 0x0000_0011,
                scope: Scope::Global,
                key: "phux.example/v1".to_owned(),
            },
        ),
        (
            "snap_list_metadata_group",
            FrameKind::ListMetadata {
                request_id: 0x0000_0012,
                scope: Scope::Group(GroupId::new(1)),
            },
        ),
        (
            "snap_subscribe_metadata_group_layout",
            FrameKind::SubscribeMetadata {
                scope: Scope::Group(GroupId::new(1)),
                key: "phux.tui.layout/v1".to_owned(),
            },
        ),
        (
            "snap_metadata_changed_set_group",
            FrameKind::MetadataChanged {
                scope: Scope::Group(GroupId::new(1)),
                key: "phux.tui.layout/v1".to_owned(),
                value: Some(b"\xa2\x01\x01\x02\x82\x00\x01".to_vec()),
                actor: None,
            },
        ),
        (
            "snap_metadata_changed_tombstone",
            FrameKind::MetadataChanged {
                scope: Scope::Global,
                key: "phux.example/v1".to_owned(),
                value: None,
                actor: None,
            },
        ),
        // L3 metadata reply frames.
        (
            "snap_metadata_value_present",
            FrameKind::MetadataValue {
                request_id: 0x0000_0007,
                value: Some(b"\xa2\x01\x01\x02\x82\x00\x01".to_vec()),
            },
        ),
        (
            "snap_metadata_value_absent",
            FrameKind::MetadataValue {
                request_id: 0x0000_0042,
                value: None,
            },
        ),
        (
            "snap_metadata_keys_empty",
            FrameKind::MetadataKeys {
                request_id: 0x0000_0012,
                keys: Vec::new(),
            },
        ),
        (
            "snap_metadata_keys_populated",
            FrameKind::MetadataKeys {
                request_id: 0x0000_0012,
                keys: vec![
                    "phux.tui.layout/v1".to_owned(),
                    "phux.tui.window_order/v1".to_owned(),
                ],
            },
        ),
        // L3 host query: LIST_DIRECTORY and its DIRECTORY_LISTING reply.
        (
            "snap_list_directory_home",
            FrameKind::ListDirectory {
                request_id: 0x0000_0021,
                path: String::new(),
                host: None,
            },
        ),
        (
            "snap_directory_listing_ok",
            FrameKind::DirectoryListing {
                request_id: 0x0000_0021,
                result: Ok(phux_protocol::wire::frame::DirectoryListing {
                    path: "/home/u".to_owned(),
                    parent: Some("/home".to_owned()),
                    entries: vec![
                        phux_protocol::wire::frame::DirectoryEntry {
                            name: "src".to_owned(),
                            is_symlink: false,
                        },
                        phux_protocol::wire::frame::DirectoryEntry {
                            name: "www".to_owned(),
                            is_symlink: true,
                        },
                    ],
                    truncated: true,
                }),
            },
        ),
        (
            "snap_directory_listing_denied",
            FrameKind::DirectoryListing {
                request_id: 0x0000_0022,
                result: Err(phux_protocol::wire::frame::DirectoryListingError {
                    path: "/root".to_owned(),
                    code: phux_protocol::wire::frame::DirectoryErrorCode::PermissionDenied,
                    message: "denied".to_owned(),
                }),
            },
        ),
    ]
}
