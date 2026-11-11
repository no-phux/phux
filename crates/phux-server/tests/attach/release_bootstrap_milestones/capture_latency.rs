//! Echoes cover synchronous encoding, before any BEGIN is received.

#![allow(
    clippy::print_stderr,
    reason = "release gate reports observed native attach latency"
)]

use super::{
    Arc, Barrier, Duration, FuturesUnordered, Instant, LaunchCommand, StreamExt, TempDir,
    WIRE_RECV_TIMEOUT, attach_through_begin, finish_attach, join_after_shutdown,
    receive_established_until, run_local, send_live_bytes, spawn_server_with_seed_cmd, timeout,
    wait_for_fullscreen_marker, wait_for_native_socket,
};

/// Keep issuing established-client echoes from the same barrier that releases
/// ATTACH. Post-BEGIN echo alone cannot measure the synchronous encoder:
/// encoding has already completed by then.
#[test]
fn established_echo_covers_native_capture_startup() {
    run_local(async {
        let tmp = TempDir::new().expect("tempdir");
        let socket = tmp.path().join("capture-startup.sock");
        let mut command = LaunchCommand::new("/bin/sh");
        command.args(["-c", r#"i=0; while [ $i -lt 50000 ]; do printf 'history-%08d α界\r\n' "$i"; i=$((i+1)); done; printf '\033[?1049h\033[2J\033[HRELEASE-FULLSCREEN'; exec /bin/cat"#]);
        let (shutdown, server) = spawn_server_with_seed_cmd(socket.clone(), "release", command);
        let mut established = wait_for_fullscreen_marker(&socket).await;
        let mut negotiated = Vec::new();
        for id in 1..=8 {
            negotiated.push((id, wait_for_native_socket(&socket).await));
        }
        let barrier = Arc::new(Barrier::new(9));
        let mut attaching = FuturesUnordered::new();
        for (id, stream) in negotiated {
            attaching.push(attach_through_begin(stream, id, Arc::clone(&barrier)));
        }
        let completed = std::cell::Cell::new(false);
        let started = Instant::now();
        let collect = async {
            let mut clients = Vec::new();
            while let Some(client) = attaching.next().await {
                clients.push(client);
            }
            completed.set(true);
            clients
        };
        let probe = async {
            barrier.wait().await;
            let mut samples = 0;
            let mut slowest = Duration::ZERO;
            loop {
                let marker = format!("PHUX-CAPTURE-STARTUP-{samples:04}\n");
                let sent = Instant::now();
                send_live_bytes(&mut established, marker.as_bytes()).await;
                receive_established_until(&mut established, marker.trim_end().as_bytes()).await;
                slowest = slowest.max(sent.elapsed());
                samples += 1;
                if completed.get() {
                    break;
                }
                tokio::task::yield_now().await;
            }
            (samples, slowest)
        };
        let (clients, (samples, slowest)) =
            timeout(WIRE_RECV_TIMEOUT, async { tokio::join!(collect, probe) })
                .await
                .expect("native capture startup stalled the established client");
        assert_eq!(clients.len(), 8);
        eprintln!(
            "native capture startup: eight BEGINs in {:?}, {samples} concurrent echoes, slowest {slowest:?}",
            started.elapsed()
        );
        for client in clients {
            drop(finish_attach(client, None).await);
        }
        drop(established);
        join_after_shutdown(shutdown, server).await;
    });
}
