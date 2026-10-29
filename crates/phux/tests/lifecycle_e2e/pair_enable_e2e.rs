//! `phux pair` on a fresh install, where `[policy] mode` is unset and no
//! remote listener is bound (PHUX-537).
//!
//! A terminal gets one yes/no question. Yes writes `mode = "paired"` and
//! restarts the server through `phux upgrade` (the pane stays). No, an
//! empty answer, and a run with no terminal leave the config alone and
//! refuse in one line. `--enable` is that yes without a question.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;
#[path = "../common/listeners.rs"]
mod listeners;

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::ServerGuard;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

const UNSET_CONFIG: &str = "\
# keep-me
[sidebar]
width = 28
";

const PAIRED_CONFIG: &str = "\
# keep-me
[sidebar]
width = 28

[policy]
mode = \"paired\"
";

/// No terminal: `phux pair` and `phux pair --qr` do not prompt, do not
/// touch the config, and refuse in one line naming `--enable` and the
/// setting.
#[test]
#[ignore = "spawns a real phux server; run via `just e2e`."]
fn pair_without_a_terminal_names_enable_and_leaves_the_config() {
    let config = tempfile::tempdir().expect("config");
    let path = write_config(config.path(), UNSET_CONFIG);
    let server = ServerGuard::builder("pairno")
        .env("XDG_CONFIG_HOME", config.path())
        .start();

    for args in [vec!["pair"], vec!["pair", "--qr"]] {
        let (code, stdout, stderr) = server.run(&args);
        assert_ne!(code, 0, "{args:?} should refuse: {stderr}");
        assert!(stdout.is_empty(), "{args:?} minted nothing: {stdout}");
        assert!(
            !stderr.contains("phones need paired mode"),
            "{args:?} prompted: {stderr}"
        );
        assert!(
            !stderr.contains("PHUX_WS_ADDR") && !stderr.contains("overlay network"),
            "{args:?} still prints the old refusal: {stderr}"
        );
        let errors: Vec<_> = stderr
            .lines()
            .filter(|line| line.contains("phux: error:"))
            .collect();
        assert_eq!(errors.len(), 1, "{args:?}: {stderr}");
        let line = errors[0];
        assert!(!line.contains('\n'));
        assert!(
            line.contains("phux pair --enable") && line.contains("[policy] mode = \"paired\""),
            "{line}"
        );
        assert!(line.contains("phux doctor"), "{line}");
        assert_eq!(
            std::fs::read_to_string(&path).expect("config"),
            UNSET_CONFIG
        );
    }
}

/// `--json` on a terminal does not ask, even though stdin is a tty. The
/// refusal is the one line naming `--enable`, and the config is unchanged.
#[test]
#[ignore = "spawns a real phux server and a pty; run via `just e2e`."]
fn pair_json_on_a_terminal_names_enable_and_leaves_the_config() {
    let config = tempfile::tempdir().expect("config");
    let path = write_config(config.path(), UNSET_CONFIG);
    let server = ServerGuard::builder("pairjson")
        .env("XDG_CONFIG_HOME", config.path())
        .start();

    // A yes is queued only if a prompt appears. The command must exit
    // before that, so the reply is never consumed.
    let (code, transcript) = drive_pty(&server, &["--json"], "phones need paired mode", "y\n");
    assert_ne!(code, Some(0), "{transcript}");
    assert!(
        !transcript.contains("phones need paired mode"),
        "json prompted:\n{transcript}"
    );
    assert!(
        transcript.contains("phux pair --enable")
            && transcript.contains("[policy] mode = \"paired\""),
        "{transcript}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("config"),
        UNSET_CONFIG
    );
}

/// A terminal asks once. `n` and an empty answer leave the config alone
/// and print the same one-line refusal. This is `phux pair --qr`, the
/// first-phone command.
#[test]
#[ignore = "spawns a real phux server and a pty; run via `just e2e`."]
fn pair_on_a_terminal_declines_without_changing_the_config() {
    let config = tempfile::tempdir().expect("config");
    let path = write_config(config.path(), UNSET_CONFIG);
    let server = ServerGuard::builder("pairask")
        .env("XDG_CONFIG_HOME", config.path())
        .start();

    for reply in ["n\n", "\n"] {
        let (code, transcript) = drive_pty(&server, &["--qr"], "phones need paired mode", reply);
        assert_ne!(
            code,
            Some(0),
            "reply {reply:?} should refuse:\n{transcript}"
        );
        assert!(
            transcript.contains(
                "phones need paired mode: the server will listen on your tailnet for enrolled devices only. turn it on and restart the server? (panes are kept) [y/N] "
            ),
            "{transcript}"
        );
        assert!(
            transcript.contains("phux pair --enable")
                && transcript.contains("[policy] mode = \"paired\""),
            "{transcript}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("config"),
            UNSET_CONFIG
        );
    }
}

/// `--enable` with no terminal writes `paired`, restarts the server, and
/// mints. The seed pane is still alive and the comment in the config stays.
#[test]
#[ignore = "spawns a real phux server and re-execs it; run via `just e2e`."]
fn pair_enable_writes_paired_mode_and_mints() {
    let restarted = server_with_unbound_listener("pairon");
    let (code, stdout, stderr) = restarted.server.run(&["pair", "--enable"]);
    assert_eq!(code, 0, "stderr:\n{stderr}\nstdout:\n{stdout}");
    assert!(
        stdout.contains("Token written to"),
        "the mint should continue after the restart:\n{stdout}"
    );
    assert_config_paired(&restarted.config_path);
    assert!(
        common::process_exists(restarted.child),
        "the seed pane should survive the restart"
    );
    listeners::bound_listener_addr(
        &restarted.server.socket,
        listeners::RemoteListenerTransport::Quic,
    );
}

/// Answering yes is the same path as `--enable`: the config gains
/// `mode = "paired"`, the server restarts, and the mint continues.
#[test]
#[ignore = "spawns a real phux server, a pty, and re-execs the server; run via `just e2e`."]
fn pair_on_a_terminal_enables_paired_mode_and_mints() {
    let restarted = server_with_unbound_listener("pairyes");
    let (code, transcript) = drive_pty(&restarted.server, &[], "phones need paired mode", "y\n");
    assert_eq!(code, Some(0), "{transcript}");
    assert!(
        transcript.contains("Token written to"),
        "the mint should continue after yes:\n{transcript}"
    );
    assert_config_paired(&restarted.config_path);
    assert!(
        common::process_exists(restarted.child),
        "the seed pane should survive the restart"
    );
    listeners::bound_listener_addr(
        &restarted.server.socket,
        listeners::RemoteListenerTransport::Quic,
    );
}

/// A server that bound `--quic` under `paired`, then lost the mode across
/// an upgrade, so the listener is unbound and the seed pane is still alive.
struct Restarted {
    server: ServerGuard,
    config_path: PathBuf,
    child: u32,
    _config: tempfile::TempDir,
}

fn server_with_unbound_listener(prefix: &str) -> Restarted {
    let config = tempfile::tempdir().expect("config");
    let path = write_config(config.path(), PAIRED_CONFIG);
    let server = ServerGuard::builder(prefix)
        .seed_command("exec sleep 600")
        .env("XDG_CONFIG_HOME", config.path())
        .env("PHUX_TAILSCALE", config.path().join("no-such-tailscale"))
        .start_with(|cmd| {
            cmd.args(["--quic", listeners::LOOPBACK_ANY_PORT]);
        });
    listeners::bound_listener_addr(&server.socket, listeners::RemoteListenerTransport::Quic);
    let child = wait_child(server.pid());
    std::fs::write(&path, UNSET_CONFIG).expect("drop the mode");
    let (code, _stdout, stderr) = server.run(&["upgrade"]);
    assert_eq!(code, 0, "upgrade should ack: {stderr}");
    let report = listeners::listener_report(&server.socket);
    let slot = report
        .listeners
        .iter()
        .find(|slot| slot.transport == listeners::RemoteListenerTransport::Quic)
        .unwrap_or_else(|| panic!("no quic slot in {report:?}"));
    assert!(!slot.bound, "the listener should be unbound: {slot:?}");
    assert!(
        common::process_exists(child),
        "the pane should survive the upgrade"
    );
    Restarted {
        server,
        config_path: path,
        child,
        _config: config,
    }
}

fn write_config(home: &Path, body: &str) -> PathBuf {
    let path = home.join("phux/config.toml");
    std::fs::create_dir_all(path.parent().expect("parent")).expect("config dir");
    std::fs::write(&path, body).expect("write config");
    path
}

fn assert_config_paired(path: &Path) {
    let written = std::fs::read_to_string(path).expect("config");
    assert!(written.contains("# keep-me"), "{written}");
    assert!(written.contains("width = 28"), "{written}");
    assert!(written.contains("mode = \"paired\""), "{written}");
}

fn wait_child(parent: u32) -> u32 {
    let start = Instant::now();
    loop {
        if let Some(pid) = child_of(parent) {
            return pid;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "the seed pane's child never spawned"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn child_of(parent: u32) -> Option<u32> {
    let out = std::process::Command::new("pgrep")
        .args(["-P", &parent.to_string()])
        .stdout(Stdio::piped())
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .and_then(|line| line.trim().parse().ok())
}

/// Run `phux pair` on a pty. Wait until `needle` is visible, write `reply`,
/// then wait for the process to exit. Returns the exit code and the visible
/// transcript.
fn drive_pty(
    server: &ServerGuard,
    args: &[&str],
    needle: &str,
    reply: &str,
) -> (Option<i32>, String) {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 40,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");
    let mut cmd = CommandBuilder::new(crate::runner::phux_bin());
    for key in common::ambient_phux_keys() {
        cmd.env_remove(key);
    }
    cmd.env("XDG_STATE_HOME", &server.xdg.state);
    cmd.env("XDG_CONFIG_HOME", &server.xdg.config);
    cmd.env(common::NO_UPDATE_CHECK_ENV, common::NO_UPDATE_CHECK_VALUE);
    cmd.env("TERM", "xterm-256color");
    cmd.arg("pair");
    cmd.arg("--socket");
    cmd.arg(&server.socket);
    cmd.args(args);
    let mut child = pair.slave.spawn_command(cmd).expect("spawn phux pair");
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("clone reader");
    let mut writer = pair.master.take_writer().expect("pty writer");
    let transcript = Arc::new(Mutex::new(Vec::<u8>::new()));
    let sink = Arc::clone(&transcript);
    std::thread::spawn(move || {
        let mut buf = [0_u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            sink.lock()
                .expect("transcript")
                .extend_from_slice(&buf[..n]);
        }
    });

    let visible = |bytes: &[u8]| common::strip_terminal_controls(bytes);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let text = visible(&transcript.lock().expect("transcript"));
        if text.contains(needle) {
            break;
        }
        if let Some(status) = child.try_wait().expect("poll pair") {
            std::thread::sleep(Duration::from_millis(100));
            let text = visible(&transcript.lock().expect("transcript"));
            return (Some(exit_code(&status)), text);
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            return (None, text);
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    writer.write_all(reply.as_bytes()).expect("write reply");
    writer.flush().expect("flush reply");

    let deadline = Instant::now() + Duration::from_secs(45);
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll pair") {
            break Some(status);
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(30));
    };
    std::thread::sleep(Duration::from_millis(100));
    let text = visible(&transcript.lock().expect("transcript"));
    let code = status.as_ref().map(exit_code);
    (code, text)
}

fn exit_code(status: &portable_pty::ExitStatus) -> i32 {
    i32::try_from(status.exit_code()).unwrap_or(-1)
}
