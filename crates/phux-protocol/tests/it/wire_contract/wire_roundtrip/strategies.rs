//! Proptest strategies for snapshot records and the handshake, simple, and attach frames.

use proptest::prelude::*;

use super::{
    AgentFacet, AttachTarget, BootstrapId, ClientCapabilities, ClientId, ColorSupport, FrameKind,
    ResourceId, ResourceInfo, ResourceKind, SessionId, SessionInfo, SessionSnapshot, StreamId,
    ViewportInfo, WindowId, WindowInfo, arb_detach_reason, arb_error_code, arb_frame_kind_input,
    arb_frame_kind_lifecycle, arb_frame_kind_metadata, arb_layer_set, arb_terminal_id,
    arb_vt_bytes,
};

pub(super) fn arb_attach_target() -> impl Strategy<Value = AttachTarget> {
    prop_oneof![
        Just(AttachTarget::Last),
        ".{0,64}".prop_map(AttachTarget::ByName),
        any::<u32>().prop_map(|id| AttachTarget::ById(SessionId::new(id))),
        (
            ".{0,32}",
            proptest::option::of(proptest::collection::vec(".{0,16}", 0..4)),
            proptest::option::of(".{0,32}"),
        )
            .prop_map(|(name, command, cwd)| AttachTarget::CreateIfMissing {
                name,
                command,
                cwd,
            }),
    ]
}

pub(super) fn arb_viewport_info() -> impl Strategy<Value = ViewportInfo> {
    (
        any::<u16>(),
        any::<u16>(),
        proptest::option::of(any::<u16>()),
        proptest::option::of(any::<u16>()),
    )
        .prop_map(|(cols, rows, pixel_w, pixel_h)| {
            ViewportInfo::new(cols, rows).with_pixels(pixel_w, pixel_h)
        })
}

pub(super) fn arb_session_info() -> impl Strategy<Value = SessionInfo> {
    (
        any::<u32>(),
        ".{0,32}",
        proptest::option::of(any::<u32>()),
        any::<i64>(),
        any::<u16>(),
        any::<u16>(),
    )
        .prop_map(
            |(
                id,
                name,
                active_window,
                created_at_unix_secs,
                window_count,
                attached_client_count,
            )| {
                SessionInfo::new(SessionId::new(id), name)
                    .with_active_window(active_window.map(WindowId::new))
                    .with_created_at_unix_secs(created_at_unix_secs)
                    .with_window_count(window_count)
                    .with_attached_client_count(attached_client_count)
            },
        )
}

pub(super) fn arb_window_info() -> impl Strategy<Value = WindowInfo> {
    (
        any::<u32>(),
        any::<u32>(),
        any::<u16>(),
        ".{0,32}",
        proptest::option::of(any::<u32>()),
    )
        .prop_map(|(id, session_id, index, name, active_resource)| {
            WindowInfo::new(WindowId::new(id), SessionId::new(session_id), name)
                .with_index(index)
                .with_active_resource(active_resource.map(ResourceId::local))
        })
}

pub(super) fn arb_pane_info() -> impl Strategy<Value = ResourceInfo> {
    (
        any::<u32>(),
        any::<u32>(),
        any::<u16>(),
        any::<u16>(),
        proptest::option::of(".{0,32}"),
        proptest::option::of(".{0,32}"),
    )
        .prop_map(|(id, window_id, cols, rows, title, cwd)| {
            ResourceInfo::new(ResourceId::local(id), WindowId::new(window_id), cols, rows)
                .with_title(title)
                .with_cwd(cwd)
        })
}

pub(super) fn arb_resource_kind() -> impl Strategy<Value = ResourceKind> {
    prop_oneof![
        Just(ResourceKind::Terminal),
        Just(ResourceKind::AgentSession),
        (2_u8..=u8::MAX).prop_map(|tag| ResourceKind::Unknown { tag }),
    ]
}

pub(super) fn arb_agent_facet() -> impl Strategy<Value = AgentFacet> {
    (".{0,16}", proptest::option::of(".{0,32}"), ".{0,16}").prop_map(
        |(provider, native_id, state)| AgentFacet::new(provider, state).with_native_id(native_id),
    )
}

/// A snapshot entry carrying resource facets: any kind, an optional parent,
/// an optional agent facet. Ids are drawn from a small space so a snapshot
/// of several entries still has distinct ids (the facet join is by id).
pub(super) fn arb_resource_info() -> impl Strategy<Value = ResourceInfo> {
    (
        arb_pane_info(),
        arb_resource_kind(),
        proptest::option::of(arb_terminal_id()),
        proptest::option::of(arb_agent_facet()),
    )
        .prop_map(|(info, kind, parent, agent)| {
            info.with_kind(kind).with_parent(parent).with_agent(agent)
        })
}

pub(super) fn arb_session_snapshot() -> impl Strategy<Value = SessionSnapshot> {
    (
        proptest::collection::vec(arb_session_info(), 0..3),
        proptest::collection::vec(arb_window_info(), 0..4),
        proptest::collection::vec(arb_pane_info(), 0..5),
        any::<u32>(),
        any::<u32>(),
        any::<u32>(),
        proptest::option::of(any::<u64>()),
    )
        .prop_map(|(sessions, windows, panes, fs, fw, fp, head)| {
            SessionSnapshot::new(SessionId::new(fs), WindowId::new(fw), ResourceId::new(fp))
                .with_sessions(sessions)
                .with_windows(windows)
                .with_resources(panes)
                .with_journal_head(head)
        })
}

pub(super) fn arb_color_support() -> impl Strategy<Value = ColorSupport> {
    prop_oneof![
        Just(ColorSupport::TrueColor),
        Just(ColorSupport::Indexed256),
        Just(ColorSupport::Indexed16),
        Just(ColorSupport::Mono),
    ]
}

/// Non-zero stream / bootstrap generation ids. Protocol 0.7 binds
/// `ResourceOutput` and `FrameAck` to these; zero is reserved.
pub(super) fn arb_stream_id() -> impl Strategy<Value = StreamId> {
    (1_u64..=u64::MAX).prop_map(|raw| StreamId::new(raw).unwrap())
}

pub(super) fn arb_bootstrap_id() -> impl Strategy<Value = BootstrapId> {
    (1_u64..=u64::MAX).prop_map(|raw| BootstrapId::new(raw).unwrap())
}

/// Strategy over the thin-wrapper `FrameKind` catalog. Focused tests below
/// keep constraints this union cannot express: `prop_assume!` agent-session
/// facet bounds, unknown spawn kinds, `bind_instance`, `KILL_RESOURCE_IF`
/// condition bits, bootstrap/history lifecycle, and snapshot-component
/// isolation (session/window/pane/layout/resource-info).
pub(super) fn arb_frame_kind() -> impl Strategy<Value = FrameKind> {
    prop_oneof![
        arb_frame_kind_simple(),
        arb_frame_kind_attach(),
        arb_frame_kind_input(),
        arb_frame_kind_metadata(),
        arb_frame_kind_lifecycle(),
    ]
}

pub(super) fn arb_frame_kind_simple() -> impl Strategy<Value = FrameKind> {
    prop_oneof![
        (
            ".{0,128}",
            any::<u16>(),
            any::<u16>(),
            any::<u16>(),
            arb_color_support(),
            arb_layer_set(),
        )
            .prop_map(
                |(client_name, major, minor, patch, color_support, layers)| {
                    FrameKind::Hello {
                        client_name,
                        protocol_major: major,
                        protocol_minor: minor,
                        protocol_patch: patch,
                        client_caps: ClientCapabilities::new()
                            .with_color_support(color_support)
                            .with_layers(layers),
                    }
                }
            ),
        any::<u64>().prop_map(|nonce| FrameKind::Ping { nonce }),
        any::<u64>().prop_map(|nonce| FrameKind::Pong { nonce }),
        Just(FrameKind::Detach),
        (arb_detach_reason(), ".{0,128}")
            .prop_map(|(reason, message)| FrameKind::Detached { reason, message }),
        arb_terminal_id().prop_map(|terminal_id| FrameKind::Bell { terminal_id }),
        (
            proptest::option::of(any::<u32>()),
            arb_error_code(),
            ".{0,256}",
        )
            .prop_map(|(request_id, code, message)| FrameKind::Error {
                request_id,
                code,
                message,
            }),
        arb_viewport_info().prop_map(|viewport| FrameKind::ViewportResize { viewport }),
    ]
}

pub(super) fn arb_frame_kind_attach() -> impl Strategy<Value = FrameKind> {
    prop_oneof![
        (
            arb_terminal_id(),
            arb_stream_id(),
            arb_bootstrap_id(),
            any::<u64>(),
            arb_vt_bytes(),
        )
            .prop_map(|(terminal_id, stream_id, bootstrap_id, seq, bytes)| {
                FrameKind::ResourceOutput {
                    terminal_id,
                    stream_id,
                    bootstrap_id,
                    seq,
                    bytes: bytes.into(),
                }
            }),
        (
            arb_terminal_id(),
            arb_stream_id(),
            arb_bootstrap_id(),
            any::<u64>(),
        )
            .prop_map(
                |(terminal_id, stream_id, bootstrap_id, seq)| FrameKind::FrameAck {
                    terminal_id,
                    stream_id,
                    bootstrap_id,
                    seq,
                }
            ),
        (
            any::<u32>(),
            arb_attach_target(),
            arb_viewport_info(),
            any::<bool>(),
            any::<u32>(),
            proptest::option::of(any::<u8>()),
        )
            .prop_map(
                |(
                    attach_id,
                    target,
                    viewport,
                    request_scrollback,
                    scrollback_limit_lines,
                    role_byte,
                )| {
                    FrameKind::Attach {
                        attach_id,
                        target,
                        viewport,
                        request_scrollback,
                        scrollback_limit_lines,
                        role_policy: role_byte.map(phux_protocol::wire::frame::RolePolicy::from_u8),
                    }
                },
            ),
        (any::<u32>(), arb_session_snapshot(), any::<u32>()).prop_map(
            |(attach_id, snapshot, client_id)| FrameKind::Attached {
                attach_id,
                snapshot,
                initial_client_id: ClientId::new(client_id),
            },
        ),
    ]
}
