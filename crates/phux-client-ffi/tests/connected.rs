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
    ABI_VERSION, PhuxAttachOptions, PhuxBytes, PhuxCatalogTerminal, PhuxClient, PhuxClientOptions,
    PhuxClientResult, PhuxClientState, PhuxConnectOptions, PhuxResourceId, PhuxSessionInfo,
    PhuxTerminalGridView, PhuxWorkspaceInfo, PhuxWorkspaceMutation, PhuxWorkspaceWindow,
    phux_client_catalog_terminal_get, phux_client_connect, phux_client_connection_epoch,
    phux_client_connection_error, phux_client_feed_frame, phux_client_free,
    phux_client_is_connected, phux_client_new, phux_client_outgoing_count, phux_client_poll,
    phux_client_poll_pending, phux_client_queue_attach, phux_client_resource_count,
    phux_client_send_paste, phux_client_session_get, phux_client_state, phux_client_terminal_grid,
    phux_client_workspace_info, phux_client_workspace_mutate, phux_client_workspace_refresh,
    phux_client_workspace_window_get,
};
use phux_server_testkit::{run_local, spawn_server, spawn_server_with_seed_cmd};
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

#[derive(Default)]
struct WakeCounter(AtomicUsize);

struct ConnectedClientGuard(*mut PhuxClient);

impl Drop for ConnectedClientGuard {
    fn drop(&mut self) {
        // SAFETY: this guard uniquely owns the pointer returned by connect;
        // the C export accepts null when setup did not complete.
        unsafe { phux_client_free(self.0) };
    }
}

unsafe extern "C" fn count_wake(context: *mut c_void) {
    // SAFETY: `connected_poll_rearms_wake_for_later_attach_activity` retains
    // this counter until `phux_client_free` has joined the driver.
    let counter = unsafe { &*context.cast::<WakeCounter>() };
    counter.0.fetch_add(1, Ordering::Release);
}

async fn wait_for_wakes(counter: &WakeCounter, expected: usize, what: &str) {
    let start = Instant::now();
    while counter.0.load(Ordering::Acquire) < expected {
        assert!(
            start.elapsed() < DEADLINE,
            "timed out waiting for {what} (wakes: {})",
            counter.0.load(Ordering::Acquire)
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
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

#[test]
fn connected_poll_rearms_wake_for_later_attach_activity() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let socket_text = socket.to_str().unwrap().to_owned();
        let (shutdown, server) = spawn_server(socket.clone(), Some("main"));
        let counter = Box::new(WakeCounter::default());
        // Declare the client after its callback context so unwinding joins the
        // driver before the counter can be dropped.
        let mut client = ConnectedClientGuard(std::ptr::null_mut());

        let options = PhuxConnectOptions {
            size: std::mem::size_of::<PhuxConnectOptions>(),
            version: ABI_VERSION,
            base: base_options(),
            target: PhuxBytes::default(),
            socket_path: span(&socket_text),
            config_path: PhuxBytes::default(),
            client_name: span("phux-client-ffi-connected-rearm"),
            wake: Some(count_wake),
            wake_context: (&raw const *counter).cast_mut().cast(),
        };
        // SAFETY: readable options whose spans outlive the call, writable out.
        assert_eq!(
            unsafe { phux_client_connect(&raw const options, &raw mut client.0) },
            PhuxClientResult::Ok,
            "connect"
        );

        wait_for_wakes(&counter, 1, "the runtime's initial wake").await;
        // Polling the listener-registration wake has no socket activity to
        // consume. It must nevertheless re-arm the runtime for HELLO_OK.
        // SAFETY: a live client, on its owning thread.
        assert_eq!(unsafe { phux_client_poll(client.0) }, PhuxClientResult::Ok);
        let mut expected_wakes = 1;
        while !matches!(
            // SAFETY: a live client.
            unsafe { phux_client_state(client.0) },
            PhuxClientState::Negotiated
        ) {
            expected_wakes += 1;
            wait_for_wakes(&counter, expected_wakes, "a lifecycle wake after a poll").await;
            // SAFETY: a live client, on its owning thread.
            assert_eq!(unsafe { phux_client_poll(client.0) }, PhuxClientResult::Ok);
        }
        // SAFETY: a live client.
        assert_eq!(
            unsafe { phux_client_state(client.0) },
            PhuxClientState::Negotiated
        );

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
        assert_eq!(
            unsafe { phux_client_queue_attach(client.0, &raw const attach) },
            PhuxClientResult::Ok,
            "queue_attach"
        );

        while !matches!(
            // SAFETY: a live client.
            unsafe { phux_client_state(client.0) },
            PhuxClientState::Attached
        ) {
            expected_wakes += 1;
            wait_for_wakes(&counter, expected_wakes, "the attach response wake").await;
            // SAFETY: a live client, on its owning thread.
            assert_eq!(unsafe { phux_client_poll(client.0) }, PhuxClientResult::Ok);
        }

        drop(client);
        drop(shutdown);
        server.await.unwrap().unwrap();
    });
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

    // A drained client has nothing left to poll, which is what a consumer
    // polling its clients in turn skips an empty turn on.
    // SAFETY: a live client.
    assert_eq!(unsafe { phux_client_poll(client) }, PhuxClientResult::Ok);
    // SAFETY: a live client.
    assert!(
        !unsafe { phux_client_poll_pending(client) },
        "nothing is retained once a poll has drained it"
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

fn workspace_info(client: *mut PhuxClient) -> PhuxWorkspaceInfo {
    let mut info = PhuxWorkspaceInfo::default();
    // SAFETY: owning-thread live client and initialized, disjoint output.
    assert_eq!(
        unsafe { phux_client_workspace_info(client, &raw mut info) },
        PhuxClientResult::Ok
    );
    info
}

fn workspace_window(client: *mut PhuxClient) -> PhuxWorkspaceWindow {
    let mut window = PhuxWorkspaceWindow::default();
    // SAFETY: owning-thread live client and initialized, disjoint output.
    assert_eq!(
        unsafe { phux_client_workspace_window_get(client, 0, &raw mut window) },
        PhuxClientResult::Ok
    );
    window
}

fn copy_span(bytes: PhuxBytes) -> String {
    if bytes.len == 0 {
        return String::new();
    }
    // SAFETY: the tests copy a borrowed ABI span before any mutable client call.
    String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(bytes.data, bytes.len) })
        .into_owned()
}

fn grid_text(client: *mut PhuxClient, terminal: &PhuxResourceId) -> String {
    let mut grid = PhuxTerminalGridView::default();
    // SAFETY: client, ID and initialized output are live and disjoint.
    let result = unsafe { phux_client_terminal_grid(client, terminal, &raw mut grid) };
    if result == PhuxClientResult::NoValue {
        return String::new();
    }
    assert_eq!(result, PhuxClientResult::Ok);
    // SAFETY: successful grid acquisition owns these spans until the next
    // mutable client call; copy all text before returning.
    let cells = unsafe { std::slice::from_raw_parts(grid.cells, grid.cell_count) };
    let arena = if grid.utf8.len == 0 {
        &[]
    } else {
        // SAFETY: as above.
        unsafe { std::slice::from_raw_parts(grid.utf8.data, grid.utf8.len) }
    };
    let mut text = String::new();
    for row in cells.chunks(usize::from(grid.cols)) {
        for cell in row {
            let start = cell.utf8_offset as usize;
            let end = start + usize::from(cell.utf8_len);
            text.push_str(&String::from_utf8_lossy(&arena[start..end]));
        }
        text.push('\n');
    }
    text
}

fn paste(client: *mut PhuxClient, terminal: &PhuxResourceId, text: &str) -> PhuxClientResult {
    // SAFETY: owning-thread live client, readable ID and text for this call.
    unsafe { phux_client_send_paste(client, terminal, text.as_ptr(), text.len(), true) }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one retained-handle server-restart scenario"
)]
fn retained_handle_recovers_workspace_catalog_and_seed_terminal_after_restart() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let socket_text = socket.to_str().unwrap();
        let mut client = ConnectedClientGuard(std::ptr::null_mut());
        let mut previous_epoch = 0;
        let mut previous_terminal = None;
        let mut previous_revision = 0;

        for (generation, session) in [(1, "before"), (2, "after")] {
            let (shutdown, server) = spawn_server_with_seed_cmd(
                socket.clone(),
                session,
                portable_pty::CommandBuilder::new("/bin/sh"),
            );
            if generation == 1 {
                let options = PhuxConnectOptions {
                    size: std::mem::size_of::<PhuxConnectOptions>(),
                    version: ABI_VERSION,
                    base: base_options(),
                    target: PhuxBytes::default(),
                    socket_path: span(socket_text),
                    config_path: PhuxBytes::default(),
                    client_name: span("ffi-restart"),
                    wake: None,
                    wake_context: std::ptr::null_mut(),
                };
                // SAFETY: readable options and spans, writable client output.
                assert_eq!(
                    unsafe { phux_client_connect(&raw const options, &raw mut client.0) },
                    PhuxClientResult::Ok
                );
            }
            poll_until(client.0, "replacement negotiation", || {
                // SAFETY: live client on its owning thread.
                unsafe {
                    phux_client_connection_epoch(client.0) > previous_epoch
                        && phux_client_state(client.0) == PhuxClientState::Negotiated
                }
            })
            .await;
            // SAFETY: live client.
            previous_epoch = unsafe { phux_client_connection_epoch(client.0) };
            let attach = PhuxAttachOptions {
                size: std::mem::size_of::<PhuxAttachOptions>(),
                version: ABI_VERSION,
                attach_id: generation,
                target_kind: 1,
                session_id: 0,
                name: span(session),
                cols: 80,
                rows: 24,
                has_pixel_size: false,
                pixel_width: 0,
                pixel_height: 0,
                request_scrollback: false,
                scrollback_limit_lines: 0,
            };
            // SAFETY: live client and readable attach options.
            assert_eq!(
                unsafe { phux_client_queue_attach(client.0, &raw const attach) },
                PhuxClientResult::Ok
            );
            poll_until(client.0, "fresh workspace metadata and catalog", || {
                let info = workspace_info(client.0);
                info.revision > previous_revision && info.state == 1 && info.status == 2
            })
            .await;
            let info = workspace_info(client.0);
            assert_eq!((info.window_count, info.terminal_count), (1, 1));
            let mut catalog = PhuxCatalogTerminal::default();
            let mut listed_session = PhuxSessionInfo::default();
            // SAFETY: live client and initialized disjoint outputs.
            unsafe {
                assert_eq!(
                    phux_client_catalog_terminal_get(client.0, 0, &raw mut catalog),
                    PhuxClientResult::Ok
                );
                assert_eq!(
                    phux_client_session_get(client.0, 0, &raw mut listed_session),
                    PhuxClientResult::Ok
                );
            }
            assert_eq!(copy_span(listed_session.name), session);
            let terminal = catalog.terminal_id;
            assert_eq!(terminal.kind, 0);
            if let Some(previous) = previous_terminal {
                assert_eq!(terminal.id, previous, "restart reuses the seed resource ID");
                assert_ne!(copy_span(workspace_window(client.0).name), "retired-layout");
            }
            let marker = format!("FFI_RECOVERY_{generation}");
            // The full marker is absent from the shell command itself, so
            // observing it proves shell execution, not merely PTY input echo.
            let command = format!("printf '%s%s\\n' 'FFI_RECOVERY_' '{generation}'\r");
            assert_eq!(paste(client.0, &terminal, &command), PhuxClientResult::Ok);
            poll_until(client.0, "fresh seed terminal input/output", || {
                grid_text(client.0, &terminal).contains(&marker)
            })
            .await;

            if generation == 1 {
                let window = workspace_window(client.0);
                let mutation = PhuxWorkspaceMutation {
                    request_id: 1,
                    expected_revision: info.revision,
                    session_id: info.session_id,
                    kind: 6,
                    window_id: window.window_id,
                    name: span("retired-layout"),
                    ..PhuxWorkspaceMutation::default()
                };
                // SAFETY: readable mutation and live exclusively accessed client.
                assert_eq!(
                    unsafe { phux_client_workspace_mutate(client.0, &raw const mutation) },
                    PhuxClientResult::Ok
                );
                poll_until(client.0, "confirmed old-server workspace", || {
                    let info = workspace_info(client.0);
                    info.state == 2 && info.status == 2
                })
                .await;
                assert_eq!(copy_span(workspace_window(client.0).name), "retired-layout");
            }
            if generation == 1 {
                // Never poll this read's replies. Even if they reach the socket,
                // the binding transaction is interrupted across server restart.
                // SAFETY: live client on its owning thread.
                assert_eq!(
                    unsafe { phux_client_workspace_refresh(client.0, 2) },
                    PhuxClientResult::Ok
                );
            }
            previous_revision = workspace_info(client.0).revision;
            previous_terminal = Some(terminal.id);
            drop(shutdown);
            tokio::time::timeout(DEADLINE, server)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            poll_until(
                client.0,
                "transport loss without freeing the client",
                || {
                    // SAFETY: live client.
                    unsafe { phux_client_state(client.0) != PhuxClientState::Attached }
                },
            )
            .await;
            assert_eq!(
                paste(client.0, &terminal, "stale\r"),
                PhuxClientResult::InvalidState
            );
        }
    });
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
    // An embedded client has nothing to poll, ever.
    let mut embedded: *mut PhuxClient = std::ptr::null_mut();
    let base = base_options();
    // SAFETY: readable options, writable out.
    assert_eq!(
        unsafe { phux_client_new(&raw const base, &raw mut embedded) },
        PhuxClientResult::Ok
    );
    // SAFETY: a live client.
    assert!(!unsafe { phux_client_poll_pending(embedded) });
    // SAFETY: a live client.
    assert_eq!(
        unsafe { phux_client_poll(embedded) },
        PhuxClientResult::InvalidState,
        "poll is the connected lane's"
    );
    // SAFETY: a live client, uniquely owned, on its owning thread.
    unsafe { phux_client_free(embedded) };

    // Let the driver get well into a dial and a backoff.
    std::thread::sleep(Duration::from_millis(200));

    // The runtime's reason must be reachable: on this lane it is the only
    // account of why a host is unreachable, and a consumer showing a failed
    // host has nothing else to show.
    let mut reason = PhuxBytes::default();
    // SAFETY: a live client and a writable out.
    assert_eq!(
        unsafe { phux_client_connection_error(client, &raw mut reason) },
        PhuxClientResult::Ok
    );
    assert!(
        reason.len > 0,
        "the driver's dial failure is reported to the embedder"
    );

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
