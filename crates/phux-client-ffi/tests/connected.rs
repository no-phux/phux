//! The C ABI's connected lane against a real server (ADR-0133).
//!
//! `phux_client_connect` hands the socket, the reconnect ladder and the
//! framing to `phux-client-runtime`. What this proves is that an embedder
//! that owns none of those still reaches ATTACHED and still decodes every
//! frame on its own thread: the driver dials and reads, the wake fires, and
//! `phux_client_poll` walks the same per-frame path `phux_client_feed_frame`
//! walks.

#![allow(clippy::expect_used, reason = "test assertions")]
#![allow(clippy::unwrap_used, reason = "test assertions")]
#![allow(clippy::panic, reason = "test assertions")]
#![allow(
    clippy::future_not_send,
    reason = "the C client is owning-thread-only by contract; this future never leaves its thread"
)]

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use phux_client_ffi::{
    ABI_VERSION, PhuxAttachOptions, PhuxBytes, PhuxClient, PhuxClientOptions, PhuxClientResult,
    PhuxClientState, PhuxConnectOptions, phux_client_connect, phux_client_connection_epoch,
    phux_client_feed_frame, phux_client_free, phux_client_is_connected, phux_client_outgoing_count,
    phux_client_poll, phux_client_queue_attach, phux_client_resource_count, phux_client_state,
};
use phux_server_testkit::{run_local, spawn_server};
use tempfile::TempDir;

/// Generous, like the testkit's own deadlines: this drives a real server
/// under a parallel test run, and a genuine hang still fails.
const DEADLINE: Duration = Duration::from_secs(20);

const fn base_options() -> PhuxClientOptions {
    PhuxClientOptions {
        size: std::mem::size_of::<PhuxClientOptions>(),
        version: ABI_VERSION,
        max_bootstrap_chunk_bytes: 64 * 1024,
        max_history_page_bytes: 64 * 1024,
        max_history_page_rows: 500,
        max_history_cache_bytes: 4 * 1024 * 1024,
        max_history_materialized_rows: 1000,
        history_prefetch_rows: 100,
    }
}

const fn span(text: &str) -> PhuxBytes {
    PhuxBytes {
        data: text.as_ptr(),
        len: text.len(),
    }
}

/// Counts the driver's edge-triggered wakes.
static WAKES: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn on_wake(context: *mut c_void) {
    assert_eq!(
        context as usize, 0xfeed,
        "the context is handed back intact"
    );
    WAKES.fetch_add(1, Ordering::Release);
}

/// Poll until `ready`, or fail. Polling on a timer rather than only on the
/// wake keeps the test honest about progress without racing the callback.
/// The client never leaves this thread, so its owning-thread contract holds
/// inside the local set the server runs in.
async fn poll_until(client: *mut PhuxClient, what: &str, mut ready: impl FnMut() -> bool) {
    let start = Instant::now();
    while !ready() {
        assert!(
            start.elapsed() < DEADLINE,
            "timed out waiting for {what} (wakes: {})",
            WAKES.load(Ordering::Acquire)
        );
        // SAFETY: a live client, on this thread, for the call.
        let result = unsafe { phux_client_poll(client) };
        assert_eq!(result, PhuxClientResult::Ok, "poll failed while {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[test]
fn connects_attaches_and_decodes_without_owning_a_socket() {
    run_local(async { connected_lane().await });
}

async fn connected_lane() {
    let tmp = TempDir::new().unwrap();
    let socket = tmp.path().join("phux.sock");
    let socket_text = socket.to_str().unwrap().to_owned();

    // The server shares this thread's runtime; the client's driver brings
    // its own thread and its own runtime.
    let (shutdown, server) = spawn_server(socket.clone(), Some("main"));

    let options = PhuxConnectOptions {
        size: std::mem::size_of::<PhuxConnectOptions>(),
        version: ABI_VERSION,
        base: base_options(),
        target: PhuxBytes::default(),
        socket_path: span(&socket_text),
        config_path: PhuxBytes::default(),
        client_name: span("phux-client-ffi-connected"),
        wake: Some(on_wake),
        wake_context: 0xfeed as *mut c_void,
    };

    let mut client: *mut PhuxClient = std::ptr::null_mut();
    // SAFETY: readable options whose spans outlive the call, writable out.
    let result = unsafe { phux_client_connect(&raw const options, &raw mut client) };
    assert_eq!(result, PhuxClientResult::Ok, "connect");
    assert!(!client.is_null());
    // SAFETY: a live client.
    assert!(unsafe { phux_client_is_connected(client) });

    // The runtime queues HELLO itself, so reaching NEGOTIATED needs no
    // queue_hello from the embedder and no frame pump of its own.
    poll_until(client, "HELLO_OK", || {
        // SAFETY: a live client.
        matches!(
            unsafe { phux_client_state(client) },
            PhuxClientState::Negotiated
        )
    })
    .await;

    let attach = PhuxAttachOptions {
        size: std::mem::size_of::<PhuxAttachOptions>(),
        version: ABI_VERSION,
        attach_id: 1,
        target_kind: 1, // PHUX_ATTACH_BY_NAME
        session_id: 0,
        name: span("main"),
        cols: 40,
        rows: 8,
        has_pixel_size: false,
        pixel_width: 0,
        pixel_height: 0,
        request_scrollback: false,
        scrollback_limit_lines: 0,
    };
    // SAFETY: a live client and readable options.
    let result = unsafe { phux_client_queue_attach(client, &raw const attach) };
    assert_eq!(result, PhuxClientResult::Ok, "queue_attach");

    // Nothing drains outgoing here: releasing the control guard woke the
    // driver, and the driver writes the frame.
    // SAFETY: a live client.
    assert_eq!(
        unsafe { phux_client_outgoing_count(client) },
        0,
        "the connected lane never stages outgoing frames for the embedder"
    );

    poll_until(client, "ATTACHED", || {
        // SAFETY: a live client.
        matches!(
            unsafe { phux_client_state(client) },
            PhuxClientState::Attached
        )
    })
    .await;

    poll_until(client, "the session's resources", || {
        // SAFETY: a live client.
        (unsafe { phux_client_resource_count(client) }) > 0
    })
    .await;

    assert!(
        WAKES.load(Ordering::Acquire) > 0,
        "the driver woke the embedder from its own thread"
    );

    // The fence a connected embedder uses in place of a fresh client.
    // SAFETY: a live client.
    assert_eq!(
        unsafe { phux_client_connection_epoch(client) },
        1,
        "one connection has opened"
    );

    // The embedded lane's pump is refused: this client does not own bytes.
    let stray = [0_u8; 4];
    // SAFETY: a live client and a readable span.
    let refused = unsafe { phux_client_feed_frame(client, stray.as_ptr(), stray.len()) };
    assert_eq!(
        refused,
        PhuxClientResult::InvalidState,
        "feeding a connected client is a state error, not a decode attempt"
    );

    // SAFETY: a live client, uniquely owned, on its owning thread.
    unsafe { phux_client_free(client) };
    drop(shutdown);
    server.await.unwrap().unwrap();
}

/// Counts wakes for the free-while-dialing test.
static TEARDOWN_WAKES: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn on_teardown_wake(_context: *mut c_void) {
    TEARDOWN_WAKES.fetch_add(1, Ordering::Release);
}

/// Freeing must stop the driver, not merely ask it to stop.
///
/// The wake callback runs on the driver's thread and carries a context the
/// embedder owns. If `phux_client_free` returned while that thread was still
/// walking the reconnect ladder, the next wake would reach a context the
/// embedder had already freed.
#[test]
fn freeing_joins_the_driver_so_no_wake_outlives_the_client() {
    let tmp = TempDir::new().unwrap();
    // Nothing is listening, so the driver is inside its ladder for the whole
    // of this test: the interesting window for a teardown race.
    let socket = tmp.path().join("absent.sock");
    let socket_text = socket.to_str().unwrap().to_owned();

    let options = PhuxConnectOptions {
        size: std::mem::size_of::<PhuxConnectOptions>(),
        version: ABI_VERSION,
        base: base_options(),
        target: PhuxBytes::default(),
        socket_path: span(&socket_text),
        config_path: PhuxBytes::default(),
        client_name: span("phux-client-ffi-teardown"),
        wake: Some(on_teardown_wake),
        wake_context: std::ptr::null_mut(),
    };

    let mut client: *mut PhuxClient = std::ptr::null_mut();
    // SAFETY: readable options whose spans outlive the call, writable out.
    assert_eq!(
        unsafe { phux_client_connect(&raw const options, &raw mut client) },
        PhuxClientResult::Ok,
        "a host that is down is the ladder's business, not the call's"
    );

    // Let the driver get well into a dial and a backoff.
    std::thread::sleep(Duration::from_millis(50));

    // SAFETY: a live client, uniquely owned, on its owning thread.
    unsafe { phux_client_free(client) };
    let after_free = TEARDOWN_WAKES.load(Ordering::Acquire);

    // Any wake from here on would be the driver outliving the client.
    std::thread::sleep(Duration::from_millis(250));
    assert_eq!(
        TEARDOWN_WAKES.load(Ordering::Acquire),
        after_free,
        "free joined the driver; no wake reached a freed context"
    );
}
