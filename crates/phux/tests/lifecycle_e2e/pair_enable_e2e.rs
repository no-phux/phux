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

use crate::{common, listeners};

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::ServerGuard;

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
    let (_config, path, server) = unset_server("pairno");

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
        assert_config_unchanged(&path);
    }
}

/// `--json` on a terminal does not ask, even though stdin is a tty. The
/// refusal is the one line naming `--enable`, and the config is unchanged.
#[test]
#[ignore = "spawns a real phux server and a pty; run via `just e2e`."]
fn pair_json_on_a_terminal_names_enable_and_leaves_the_config() {
    let (_config, path, server) = unset_server("pairjson");

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
    assert_config_unchanged(&path);
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
    restarted.assert_paired_and_bound();
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

impl Restarted {
    /// The config gained `paired` (keeping its comment), the seed pane
    /// survived the restart, and the listener is bound again.
    fn assert_paired_and_bound(&self) {
        let written = std::fs::read_to_string(&self.config_path).expect("config");
        assert!(written.contains("# keep-me"), "{written}");
        assert!(written.contains("width = 28"), "{written}");
        assert!(written.contains("mode = \"paired\""), "{written}");
        assert!(
            common::process_exists(self.child),
            "the seed pane should survive the restart"
        );
        listeners::bound_listener_addr(
            &self.server.socket,
            listeners::RemoteListenerTransport::Quic,
        );
    }
}

/// A server whose private config leaves `[policy] mode` unset.
fn unset_server(prefix: &str) -> (tempfile::TempDir, PathBuf, ServerGuard) {
    let config = tempfile::tempdir().expect("config");
    let path = write_config(config.path(), UNSET_CONFIG);
    let server = ServerGuard::builder(prefix)
        .env("XDG_CONFIG_HOME", config.path())
        .start();
    (config, path, server)
}

fn assert_config_unchanged(path: &Path) {
    assert_eq!(std::fs::read_to_string(path).expect("config"), UNSET_CONFIG);
}

fn write_config(home: &Path, body: &str) -> PathBuf {
    let path = home.join("phux/config.toml");
    std::fs::create_dir_all(path.parent().expect("parent")).expect("config dir");
    std::fs::write(&path, body).expect("write config");
    path
}

fn wait_child(parent: u32) -> u32 {
    let start = Instant::now();
    loop {
        if let Some(pid) = common::child_of(parent) {
            return pid;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "the seed pane's child never spawned"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Run `phux pair` on a pty. Wait until `needle` is visible, write `reply`,
/// then wait for the process to exit. Returns the exit code (`None` if it
/// never exited) and the visible transcript.
fn drive_pty(
    server: &ServerGuard,
    args: &[&str],
    needle: &str,
    reply: &str,
) -> (Option<i32>, String) {
    let socket = server.socket.to_str().expect("UTF-8 socket path");
    let mut pair_args = vec!["--socket", socket];
    pair_args.extend_from_slice(args);
    let mut client = common::PtyAttach::start_client("pair", &pair_args, (120, 40), |_, cmd| {
        cmd.env("XDG_STATE_HOME", &server.xdg.state);
        cmd.env("XDG_CONFIG_HOME", &server.xdg.config);
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    while !client.painted().contains(needle) {
        if let Some(status) = client.exit_within(Duration::ZERO) {
            std::thread::sleep(Duration::from_millis(100));
            return (Some(exit_code(&status)), client.painted());
        }
        if Instant::now() > deadline {
            return (None, client.painted());
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    client.send(reply.as_bytes());
    let status = client.exit_within(Duration::from_secs(45));
    std::thread::sleep(Duration::from_millis(100));
    (status.as_ref().map(exit_code), client.painted())
}

fn exit_code(status: &portable_pty::ExitStatus) -> i32 {
    i32::try_from(status.exit_code()).unwrap_or(-1)
}
