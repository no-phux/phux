//! Native windows share server terminals, never client layout or focused input.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "PTY acceptance tests"
)]
#[path = "../common/mod.rs"]
mod common;
use common::{PtyAttach, ServerGuard};
use phux_client::{attach::connection::Connection, org};
use phux_protocol::{ids::ResourceId, wire::frame::OrgFilter};
use std::time::{Duration, Instant};

fn wait(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < deadline, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}
fn terminals(server: &ServerGuard) -> Vec<ResourceId> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut conn = Connection::connect(&server.socket).await.unwrap();
            let inventory = org::fetch(&mut conn, OrgFilter::All, 1).await.unwrap();
            let Some(session) = inventory.session_named("omarchy") else {
                return Vec::new();
            };
            inventory
                .resources
                .iter()
                .filter(|r| r.session == session.key)
                .filter_map(|r| r.live.clone())
                .collect()
        })
}
fn screen(server: &ServerGuard, id: &ResourceId) -> String {
    server.success(&["snapshot", &id.to_string()])
}
fn native(server: &ServerGuard, args: &[&str], size: (u16, u16)) -> PtyAttach {
    let socket = server.socket_text();
    let mut invocation = vec!["--socket", &socket];
    invocation.extend_from_slice(args);
    PtyAttach::start_client("native", &invocation, size, |_, _| {})
}

#[test]
#[ignore = "real server and native PTYs; run via just e2e"]
fn native_windows_isolate_input_resize_detach_and_reattach() {
    let server = ServerGuard::builder("native")
        .env("SHELL", "/bin/sh")
        .start();
    let mut first = native(&server, &["create", "--", "/bin/sh"], (101, 31));
    wait("first native terminal", || terminals(&server).len() == 1);
    let first_id = terminals(&server).remove(0);
    wait("first bootstrap", || !first.terminal_is_cooked());
    first.send(b"echo FIRST_$((6*7)); stty size\r");
    wait("first output at full emulator size", || {
        let s = screen(&server, &first_id);
        s.contains("FIRST_42") && s.contains("31 101")
    });
    assert!(
        first.painted().contains("FIRST_42"),
        "raw output reaches emulator"
    );

    let mut second = native(&server, &["create", "--", "/bin/sh"], (87, 26));
    wait("two distinct terminals in omarchy", || {
        terminals(&server).len() == 2
    });
    let second_id = terminals(&server)
        .into_iter()
        .find(|id| id != &first_id)
        .unwrap();
    second.send(b"echo SECOND_$((6*7))\r");
    wait("second input", || {
        screen(&server, &second_id).contains("SECOND_42")
    });
    assert!(!screen(&server, &first_id).contains("SECOND_42"));
    assert!(!second.painted().contains("FIRST_42"));
    first.resize(123, 38);
    first.send(b"stty size\r");
    wait("SIGWINCH reaches PTY", || {
        screen(&server, &first_id).contains("38 123")
    });
    second.send(b"stty size\r");
    wait("other PTY keeps its size", || {
        screen(&server, &second_id).contains("26 87")
    });

    first.send(b"\x1d");
    wait("detach exits client", || !first.is_running());
    assert!(first.terminal_is_cooked(), "detach restores terminal modes");
    assert_eq!(terminals(&server).len(), 2, "shell survives detach");
    let mut reattached = native(&server, &["attach", &first_id.to_string()], (99, 29));
    wait("reattach restores screen", || {
        reattached.painted().contains("FIRST_42")
    });
    reattached.send(b"echo AGAIN_$((6*7))\r");
    wait("reattached input", || {
        screen(&server, &first_id).contains("AGAIN_42")
    });
    reattached.signal(libc::SIGHUP);
    wait("closing native window detaches", || {
        !reattached.is_running()
    });
    assert!(reattached.terminal_is_cooked());
    assert_eq!(terminals(&server).len(), 2);

    second.send(b"echo SLEEPING_$((6*7)); sleep 30\r");
    wait("sleep started", || {
        screen(&server, &second_id).contains("SLEEPING_42")
    });
    second.send(b"\x03echo INTERRUPTED_$((6*7))\r");
    wait("typed Ctrl-C interrupts the child, not the client", || {
        screen(&server, &second_id).contains("INTERRUPTED_42")
    });
    second.send(b"exit\r");
    wait("process exit closes client", || !second.is_running());
    assert!(second.terminal_is_cooked());
}

#[test]
#[ignore = "real server and native PTY; run via just e2e"]
fn native_refusal_restores_terminal_and_does_not_create_a_shell() {
    let server = ServerGuard::builder("native-refuse")
        .env("SHELL", "/bin/sh")
        .start();
    let (status, _, _) = server.run(&["native", "create"]);
    assert_ne!(
        status, 0,
        "noninteractive creation must fail before side effects"
    );
    assert!(terminals(&server).is_empty());
    let mut client = native(&server, &["attach", "@4294967295"], (80, 24));
    wait("unknown target refused", || !client.is_running());
    assert!(client.terminal_is_cooked());
    assert!(terminals(&server).is_empty());
}

#[test]
#[ignore = "real server and native PTYs; run via just e2e"]
fn native_external_signals_restore_even_with_blocking_stdin_fallback() {
    let server = ServerGuard::builder("native-signals")
        .env("SHELL", "/bin/sh")
        .start();
    let mut seed = native(&server, &["create", "--", "/bin/sh"], (80, 24));
    wait("seed terminal", || terminals(&server).len() == 1);
    let id = terminals(&server).remove(0).to_string();
    wait("seed raw mode", || !seed.terminal_is_cooked());
    seed.send(b"\x1d");
    wait("seed detached", || !seed.is_running());
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGQUIT, libc::SIGTSTP] {
        let socket = server.socket_text();
        let mut client = PtyAttach::start_client(
            "native",
            &["--socket", &socket, "attach", &id],
            (80, 24),
            |_, cmd| {
                cmd.env("PHUX_TTY_READINESS", "0");
            },
        );
        wait("signal client in raw mode", || !client.terminal_is_cooked());
        client.send(b"echo READY_$((6*7))\r");
        wait("signal client bootstrap and blocking stdin read", || {
            client.painted().contains("READY_42")
        });
        client.signal(signal);
        wait("external signal exits without another keystroke", || {
            !client.is_running()
        });
        assert!(
            client.terminal_is_cooked(),
            "signal {signal} restores termios"
        );
        assert_eq!(
            terminals(&server).len(),
            1,
            "signal {signal} preserves shell"
        );
    }
}

#[test]
#[ignore = "real server and native PTY; run via just e2e"]
fn native_short_lived_program_exits_cleanly_even_if_attach_loses_the_race() {
    let server = ServerGuard::builder("native-short")
        .env("SHELL", "/bin/sh")
        .start();
    let mut client = native(
        &server,
        &["create", "--", "/bin/sh", "-c", "printf 'FINAL_%s\\n' 42"],
        (80, 24),
    );
    wait("completed native program exits client", || {
        !client.is_running()
    });
    // Create-and-place and attach are separate server operations. If the
    // command has already lost its live handle, report that rather than hang
    // or create a replacement process. Its completed record still exists.
    let listing: serde_json::Value =
        serde_json::from_str(&server.success(&["ls", "--json"])).unwrap();
    let session = listing["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "omarchy")
        .unwrap();
    assert_eq!(session["exec"]["completed"], 1);
    assert!(
        client.painted().contains("FINAL_42") || client.painted().contains("phux: error:"),
        "{}",
        client.painted()
    );
    assert!(client.terminal_is_cooked());
}

#[test]
#[ignore = "real server and concurrent native PTYs; run via just e2e"]
fn native_concurrent_creates_share_one_persistent_session() {
    let server = ServerGuard::builder("native-concurrent")
        .env("SHELL", "/bin/sh")
        .start();
    let mut first = native(&server, &["create", "--", "/bin/sh"], (80, 24));
    let mut second = native(&server, &["create", "--", "/bin/sh"], (80, 24));
    wait("concurrent creation produces two live terminals", || {
        terminals(&server).len() == 2
    });
    wait("both native clients entered raw mode", || {
        !first.terminal_is_cooked() && !second.terminal_is_cooked()
    });
    first.send(b"\x1d");
    second.send(b"\x1d");
    wait("both clients detach", || {
        !first.is_running() && !second.is_running()
    });
    let listing: serde_json::Value =
        serde_json::from_str(&server.success(&["ls", "--json"])).unwrap();
    let sessions: Vec<_> = listing["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["name"] == "omarchy")
        .collect();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["lifetime"], "persistent");
    assert_eq!(sessions[0]["exec"]["running"], 2);
    assert!(first.terminal_is_cooked() && second.terminal_is_cooked());
}
