#![cfg(unix)]
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "ignored live integration harness uses fail-fast assertions in one end-to-end flow"
)]

use std::io::{Read, Write};
use std::mem;
use std::os::unix::net::UnixStream;
use std::ptr;
use std::time::{Duration, Instant};

use phux_client_ffi::{
    ABI_VERSION, PhuxAttachOptions, PhuxBytes, PhuxClient, PhuxClientEffect, PhuxClientOptions,
    PhuxClientResult, PhuxClientState, PhuxTerminalGridView, PhuxTerminalId,
    phux_client_effect_clear, phux_client_effect_count, phux_client_effect_get,
    phux_client_feed_frame, phux_client_free, phux_client_new, phux_client_outgoing_clear,
    phux_client_outgoing_count, phux_client_outgoing_get, phux_client_queue_attach,
    phux_client_queue_hello, phux_client_send_paste, phux_client_state, phux_client_terminal_grid,
    phux_client_terminal_resize,
};

const fn span(bytes: &[u8]) -> PhuxBytes {
    PhuxBytes {
        data: if bytes.is_empty() {
            ptr::null()
        } else {
            bytes.as_ptr()
        },
        len: bytes.len(),
    }
}

fn flush_outgoing(client: *mut PhuxClient, stream: &mut UnixStream) {
    let count = unsafe { phux_client_outgoing_count(client) };
    for index in 0..count {
        let mut frame = PhuxBytes::default();
        assert_eq!(
            unsafe { phux_client_outgoing_get(client, index, &raw mut frame) },
            PhuxClientResult::Ok
        );
        let bytes = unsafe { std::slice::from_raw_parts(frame.data, frame.len) };
        stream.write_all(bytes).expect("write bridge frame");
    }
    assert_eq!(
        unsafe { phux_client_outgoing_clear(client) },
        PhuxClientResult::Ok
    );
}

fn receive_frame(stream: &mut UnixStream) -> Vec<u8> {
    let mut header = [0_u8; 4];
    stream.read_exact(&mut header).expect("read frame header");
    let body_len = u32::from_be_bytes(header) as usize;
    let mut frame = Vec::with_capacity(body_len + 4);
    frame.extend_from_slice(&header);
    frame.resize(body_len + 4, 0);
    stream.read_exact(&mut frame[4..]).expect("read frame body");
    frame
}

fn feed_one(client: *mut PhuxClient, stream: &mut UnixStream) {
    let frame = receive_frame(stream);
    assert_eq!(
        unsafe { phux_client_feed_frame(client, frame.as_ptr(), frame.len()) },
        PhuxClientResult::Ok
    );
    flush_outgoing(client, stream);
}

fn connect_until(socket: &str, deadline: Instant) -> UnixStream {
    loop {
        match UnixStream::connect(socket) {
            Ok(stream) => return stream,
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(error) => panic!("connect current phux server at {socket}: {error}"),
        }
    }
}

#[test]
#[ignore = "requires PHUX_FFI_LIVE_SOCKET pointing at a current phux server"]
fn current_server_attach_input_resize_and_grid_projection() {
    let socket = std::env::var("PHUX_FFI_LIVE_SOCKET").expect("PHUX_FFI_LIVE_SOCKET");
    let mut stream = connect_until(&socket, Instant::now() + Duration::from_secs(5));
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("read timeout");

    let options = PhuxClientOptions {
        size: mem::size_of::<PhuxClientOptions>(),
        version: ABI_VERSION,
        max_bootstrap_chunk_bytes: 0,
        max_history_page_bytes: 0,
        max_history_page_rows: 0,
        max_history_cache_bytes: 0,
        max_history_materialized_rows: 0,
        history_prefetch_rows: 0,
    };
    let mut client = ptr::null_mut();
    assert_eq!(
        unsafe { phux_client_new(&raw const options, &raw mut client) },
        PhuxClientResult::Ok
    );

    let client_name = b"phux-client-ffi live test";
    assert_eq!(
        unsafe { phux_client_queue_hello(client, span(client_name)) },
        PhuxClientResult::Ok
    );
    flush_outgoing(client, &mut stream);
    while unsafe { phux_client_state(client) } != PhuxClientState::Negotiated {
        feed_one(client, &mut stream);
    }

    let session = b"ffi-live";
    let attach = PhuxAttachOptions {
        size: mem::size_of::<PhuxAttachOptions>(),
        version: ABI_VERSION,
        attach_id: 0,
        target_kind: 3,
        session_id: 0,
        name: span(session),
        cols: 80,
        rows: 24,
        has_pixel_size: 1,
        pixel_width: 640,
        pixel_height: 384,
        request_scrollback: 1,
        scrollback_limit_lines: 5000,
    };
    assert_eq!(
        unsafe { phux_client_queue_attach(client, &raw const attach) },
        PhuxClientResult::Ok
    );
    flush_outgoing(client, &mut stream);

    let deadline = Instant::now() + Duration::from_secs(10);
    let terminal_id = loop {
        assert!(Instant::now() < deadline, "snapshot damage never arrived");
        feed_one(client, &mut stream);
        let mut found = None;
        for index in 0..unsafe { phux_client_effect_count(client) } {
            let mut effect = PhuxClientEffect::default();
            assert_eq!(
                unsafe { phux_client_effect_get(client, index, &raw mut effect) },
                PhuxClientResult::Ok
            );
            if effect.kind == 1 && effect.detail == 1 {
                assert_eq!(effect.terminal_id.kind, 0);
                found = Some(PhuxTerminalId {
                    kind: effect.terminal_id.kind,
                    id: effect.terminal_id.id,
                    host: PhuxBytes::default(),
                });
            }
        }
        assert_eq!(
            unsafe { phux_client_effect_clear(client) },
            PhuxClientResult::Ok
        );
        if let Some(id) = found {
            break id;
        }
    };

    let command = b"printf '__PHUX_FFI_LIVE__\\n'\n";
    assert_eq!(
        unsafe {
            phux_client_send_paste(
                client,
                &raw const terminal_id,
                command.as_ptr(),
                command.len(),
                true,
            )
        },
        PhuxClientResult::Ok
    );
    flush_outgoing(client, &mut stream);

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            Instant::now() < deadline,
            "live marker never reached the grid"
        );
        feed_one(client, &mut stream);
        let mut view = PhuxTerminalGridView::default();
        if unsafe { phux_client_terminal_grid(client, &raw const terminal_id, &raw mut view) }
            == PhuxClientResult::Ok
        {
            let utf8 = unsafe { std::slice::from_raw_parts(view.utf8.data, view.utf8.len) };
            if utf8
                .windows(b"__PHUX_FFI_LIVE__".len())
                .any(|window| window == b"__PHUX_FFI_LIVE__")
            {
                break;
            }
        }
    }

    assert_eq!(
        unsafe { phux_client_terminal_resize(client, &raw const terminal_id, 96, 28) },
        PhuxClientResult::Ok
    );
    flush_outgoing(client, &mut stream);

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            Instant::now() < deadline,
            "authoritative resize snapshot never reached the grid"
        );
        feed_one(client, &mut stream);
        let mut view = PhuxTerminalGridView::default();
        if unsafe { phux_client_terminal_grid(client, &raw const terminal_id, &raw mut view) }
            == PhuxClientResult::Ok
            && view.cols == 96
            && view.rows == 28
        {
            break;
        }
    }
    unsafe { phux_client_free(client) };
}
