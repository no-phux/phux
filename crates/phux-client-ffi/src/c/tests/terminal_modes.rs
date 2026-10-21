//! Published DEC modes: bracketed paste and mouse tracking.

use super::*;

#[test]
fn bracketed_paste_getter_follows_the_published_dec_2004_mode() {
    let terminal_id = phux_protocol::ResourceId::local(7);
    let c_terminal_id = PhuxResourceId {
        kind: 0,
        id: 7,
        host: PhuxBytes::default(),
    };
    let client = attaching(&[], 7);
    let mut enabled = true;
    // Before publication there is no replica to answer from.
    assert_eq!(
        unsafe {
            phux_client_terminal_bracketed_paste(client, &raw const c_terminal_id, &raw mut enabled)
        },
        PhuxClientResult::InvalidState
    );
    let snapshot = single_terminal_snapshot(terminal_id.clone(), 80, 24);
    assert_eq!(
        feed(client, &attached_frame(7, snapshot)),
        PhuxClientResult::Ok
    );
    feed_bootstrap(client, &terminal_id, (1, 1), (80, 24), b"\x1b[?2004h");
    assert_eq!(
        feed(client, &FrameKind::AttachReady { attach_id: 7 }),
        PhuxClientResult::Ok
    );
    enabled = false;
    assert_eq!(
        unsafe {
            phux_client_terminal_bracketed_paste(client, &raw const c_terminal_id, &raw mut enabled)
        },
        PhuxClientResult::Ok
    );
    assert!(enabled);
    assert_eq!(
        feed(
            client,
            &FrameKind::ResourceOutput {
                terminal_id,
                stream_id: phux_protocol::StreamId::new(1).expect("stream"),
                bootstrap_id: phux_protocol::BootstrapId::new(1).expect("bootstrap"),
                seq: 1,
                bytes: bytes::Bytes::from_static(b"\x1b[?2004l"),
            },
        ),
        PhuxClientResult::Ok
    );
    assert_eq!(
        unsafe {
            phux_client_terminal_bracketed_paste(client, &raw const c_terminal_id, &raw mut enabled)
        },
        PhuxClientResult::Ok
    );
    assert!(!enabled);
    assert_eq!(
        unsafe {
            phux_client_terminal_bracketed_paste(client, &raw const c_terminal_id, ptr::null_mut())
        },
        PhuxClientResult::InvalidArgument
    );
    unsafe { phux_client_free(client) };
}

#[test]
#[allow(clippy::too_many_lines)]
fn mouse_tracking_getter_uses_published_dec_modes_and_preserves_borrows() {
    let terminal_id = phux_protocol::ResourceId::local(7);
    let c_terminal_id = PhuxResourceId {
        kind: 0,
        id: 7,
        host: PhuxBytes::default(),
    };
    let client = attaching(&[], 7);
    let snapshot = single_terminal_snapshot(terminal_id.clone(), 80, 24);
    assert_eq!(
        feed(client, &attached_frame(7, snapshot)),
        PhuxClientResult::Ok
    );
    feed_bootstrap(client, &terminal_id, (1, 1), (80, 24), b"\x1b[?1000h");
    assert_eq!(
        feed(client, &FrameKind::AttachReady { attach_id: 7 }),
        PhuxClientResult::Ok
    );
    let inner = unsafe { &mut (*client).inner };
    assert!(inner.attached);
    inner.selection_buf.extend_from_slice(b"borrowed");
    let borrowed = inner.selection_buf.as_ptr();

    let mut enabled = false;
    assert_eq!(
        unsafe {
            phux_client_terminal_mouse_tracking(client, &raw const c_terminal_id, &raw mut enabled)
        },
        PhuxClientResult::Ok
    );
    assert!(enabled);
    assert_eq!(unsafe { &*client }.inner.selection_buf.as_ptr(), borrowed);

    assert_eq!(
        feed(
            client,
            &FrameKind::ResourceOutput {
                terminal_id,
                stream_id: phux_protocol::StreamId::new(1).expect("stream"),
                bootstrap_id: phux_protocol::BootstrapId::new(1).expect("bootstrap"),
                seq: 1,
                bytes: bytes::Bytes::from_static(b"\x1b[?1000l"),
            },
        ),
        PhuxClientResult::Ok
    );
    assert_eq!(
        unsafe {
            phux_client_terminal_mouse_tracking(client, &raw const c_terminal_id, &raw mut enabled)
        },
        PhuxClientResult::Ok
    );
    assert!(!enabled);

    assert_eq!(
        unsafe {
            phux_client_terminal_mouse_tracking(client, &raw const c_terminal_id, ptr::null_mut())
        },
        PhuxClientResult::InvalidArgument
    );
    assert_eq!(
        unsafe { phux_client_terminal_mouse_tracking(client, ptr::null(), &raw mut enabled) },
        PhuxClientResult::InvalidArgument
    );
    let unknown_id = PhuxResourceId {
        id: 8,
        ..c_terminal_id
    };
    assert_eq!(
        unsafe {
            phux_client_terminal_mouse_tracking(client, &raw const unknown_id, &raw mut enabled)
        },
        PhuxClientResult::InvalidState
    );
    assert_eq!(
        unsafe {
            phux_client_terminal_mouse_tracking(
                ptr::null(),
                &raw const c_terminal_id,
                &raw mut enabled,
            )
        },
        PhuxClientResult::InvalidArgument
    );
    unsafe { &mut *client }.inner.in_callback = true;
    enabled = true;
    assert_eq!(
        unsafe {
            phux_client_terminal_mouse_tracking(client, &raw const c_terminal_id, &raw mut enabled)
        },
        PhuxClientResult::InvalidState
    );
    assert!(enabled);
    unsafe { &mut *client }.inner.in_callback = false;
    assert_eq!(
        feed(
            client,
            &FrameKind::Detached {
                reason: None,
                message: String::new(),
            },
        ),
        PhuxClientResult::Ok
    );
    assert_eq!(
        unsafe {
            phux_client_terminal_mouse_tracking(client, &raw const c_terminal_id, &raw mut enabled)
        },
        PhuxClientResult::InvalidState
    );
    assert!(enabled);
    unsafe { phux_client_free(client) };
}
