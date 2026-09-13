//! Failure-UX dogfood: the epic's audited silent failures, replayed end to
//! end against the real binary (phux-i0e8.13.5).
//!
//! The 2026-08-01 UX audit (epic phux-i0e8) found that errors, dead keys,
//! typos, and pane death all no-op'd invisibly. The never-silent wave fixed
//! each surface; this file pins the FIXED behavior with the real `phux`
//! binary on a private UDS (run_wait_e2e-style harness: real server,
//! `ServerGuard` drop-kill, `--exit-after-idle` backstop), so the failure
//! path stays CI-enforced product behavior rather than a release ritual.
//! Original audit evidence (`file:line` as audited on 2026-08-01), per
//! scenario:
//!
//! 1. Broken config, loud server start — audited: the server swallowed a
//!    broken `config.toml` with zero output (`server.rs:113-148`). Fixed:
//!    `phux server` refuses to start, naming the config path and
//!    `phux config check` (phux-i0e8.1.1).
//! 2. Typo'd action named at check — audited: a typo'd action name logged
//!    at debug and the key died silently (`input_dispatch.rs:2560`).
//!    Fixed: `phux config check` exits 1 naming the binding, the
//!    `unknown name` fault, and a did-you-mean suggestion (phux-i0e8.3.2).
//! 3. Malformed chord does not kill keybindings — audited: one malformed
//!    chord disabled ALL keybindings including detach (`driver.rs:3709`).
//!    Fixed: the attach path builds a lenient resolver, so only the
//!    offending binding dies and `<prefix> d` still detaches
//!    (phux-i0e8.3.4).
//! 4. Pane death surfaces exit status — audited: a dying pane discarded
//!    its exit status (`server_frame.rs:1169-1216`). Fixed:
//!    `RESOURCE_CLOSED` carries it and the client prints
//!    "session ended: the last pane exited N" on teardown (phux-i0e8.2.2).
//! 5. Server SIGKILL shows the reconnect indicator — audited: a server
//!    crash was ~10s of blank screen (`attach.rs:272-341`). Fixed: the
//!    client drops to the cooked screen and announces the loss with a live
//!    countdown ("lost the server connection; waiting up to Ns...")
//!    (phux-i0e8.2.3).
//! 6. Dead-socket `--json` parses as the contract — audited: ~32 `--json`
//!    verbs had no error-path contract (epic pattern 3, INCONSISTENCY).
//!    Fixed: one JSON line on stderr — `schema_version` /
//!    `error{code,message}` / `remedy` / `exit_code` — with stdout empty
//!    (ADR-0065 section 4, phux-i0e8.8.2).
//! 7. status/logs/doctor speak with real paths — audited: three log files
//!    existed and no command or doc ever printed any path (epic pattern 2,
//!    INVISIBILITY). Fixed: `phux status`, `phux logs`, and `phux doctor`
//!    each name the canonical server-log path resolved through
//!    `phux_server::telemetry`, so the printed path and the written path
//!    can never disagree (phux-i0e8.7).
//!
//! Scenario 5 rides the lane's retry budget (`just e2e` runs serially with
//! `--retries=2`); if it still proves flaky there, demote ONLY that test to
//! the `stress` lane with a comment — the other six are deterministic.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

/// Idle lifetime for this file's harness servers, as a backstop UNDER the
/// `Drop` kill (ADR-0063): the guard cannot run if the test process is
/// `SIGKILL`ed mid-job, and what would leak is a daemon on a socket nobody
/// will ever dial again.
const SERVER_IDLE_LIMIT_SECS: &str = "600";

/// The freshly built binary under test, injected by cargo.
const PHUX: &str = env!("CARGO_BIN_EXE_phux");

/// The pre-seeded session every scenario drives against.
const SESSION: &str = "work";

/// How long to wait for a spawned server to bind its socket (cold-start
/// generous, mirroring `run_wait_e2e`).
const SOCKET_DEADLINE: Duration = Duration::from_secs(30);

/// Poll cadence for socket-file and child-exit waits.
const POLL: Duration = Duration::from_millis(50);

/// How long an attached client gets to reach a scripted state (exit,
/// output marker) before the test declares the scenario broken.
const CLIENT_DEADLINE: Duration = Duration::from_secs(20);

/// Monotonic counter so scenarios never collide on a socket path.
static COUNTER: AtomicU32 = AtomicU32::new(0);

// ---------------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------------

/// Per-scenario isolation: a private `HOME` plus XDG dirs so no test
/// reads the developer's config, host registry, or log paths, and so
/// `phux doctor` cannot inherit `PHUX_*` from a live pane (phux-vlv1).
struct Isolation {
    home: tempfile::TempDir,
    config: tempfile::TempDir,
    state: tempfile::TempDir,
}

impl Isolation {
    fn new() -> Self {
        Self {
            home: tempfile::tempdir().expect("isolated home"),
            config: tempfile::tempdir().expect("isolated config home"),
            state: tempfile::tempdir().expect("isolated state home"),
        }
    }

    /// Write `body` as this environment's `phux/config.toml`, returning
    /// the canonical path the loader will resolve.
    fn write_config(&self, body: &str) -> PathBuf {
        let dir = self.config.path().join("phux");
        std::fs::create_dir_all(&dir).expect("create phux config dir");
        let path = dir.join("config.toml");
        std::fs::write(&path, body).expect("write scenario config");
        path
    }

    /// Point `cmd` at this isolated environment.
    ///
    /// `PHUX_PROFILE=default` pins the *released* on-disk layout
    /// (`<state>/phux`, not `<state>/phux-dev`). These tests drive a debug
    /// build, which resolves the `dev` profile (ADR-0080), so without this
    /// the paths asserted below would describe a layout no user ever sees.
    fn apply(&self, cmd: &mut Command) {
        // `env_clear` drops inherited `PHUX_SOCKET` / `PHUX_WS_*` /
        // `PHUX_SERVICE_MANAGED` from a maintainer running the suite
        // inside a live pane (phux-lru0). Re-arm only the isolation
        // table, matching `first_five_minutes_e2e` / `whoami_e2e`.
        cmd.env_clear();
        if let Some(path) = std::env::var_os("PATH") {
            cmd.env("PATH", path);
        }
        if let Some(tmp) = std::env::var_os("TMPDIR") {
            cmd.env("TMPDIR", tmp);
        }
        cmd.env("HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.config.path())
            .env("XDG_STATE_HOME", self.state.path())
            .env("XDG_CACHE_HOME", self.home.path())
            .env("XDG_DATA_HOME", self.home.path())
            .env("XDG_RUNTIME_DIR", self.home.path())
            .env("PHUX_PROFILE", "default")
            // Default profile would auto-bind overlay WSS/QUIC (ADR-0081).
            // That races the host's real server for 8787/8788 and leaves
            // bind_failed slots that doctor (phux-kyna) correctly fails on.
            .env("PHUX_NO_AUTO_LISTEN", "1")
            // Overlay detect shells out to `tailscale` and then dials
            // :8787. Point the seam at a missing binary so a host with
            // tailscale on PATH cannot leak the operator's tailnet into
            // this file (phux-vlv1).
            .env(
                "PHUX_TAILSCALE",
                self.home.path().join("no-such-tailscale"),
            );
    }

    /// The canonical server-log path `phux_server::telemetry` resolves
    /// under this environment — asserted against status/logs/doctor output.
    fn server_log(&self) -> PathBuf {
        self.state.path().join("phux").join("server.log")
    }
}

/// A running `phux server` on a private socket, killed on drop.
struct ServerGuard {
    process: common::ServerProcess,
    socket: PathBuf,
    // Held to keep the socket's temp dir alive for the guard's lifetime.
    _dir: tempfile::TempDir,
}

impl ServerGuard {
    /// Spawn `phux server --session work --socket <unique>` inside `iso`
    /// and block until the socket file appears. `SHELL=/bin/sh` keeps the
    /// seed pane deterministic (no user rc noise in scenario output).
    fn start(iso: &Isolation) -> Self {
        let dir = tempfile::tempdir().expect("socket tempdir");
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        // Short name on purpose: sun_path caps UDS paths at ~104 bytes.
        let socket = dir
            .path()
            .join(format!("fx-{}-{n}.sock", std::process::id()));
        let mut cmd = Command::new(PHUX);
        cmd.args(["server", "--session", SESSION, "--socket"])
            .arg(&socket)
            .args(["--exit-after-idle", SERVER_IDLE_LIMIT_SECS])
            .env("SHELL", "/bin/sh")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        iso.apply(&mut cmd);
        let child = cmd.spawn().expect("spawn phux server");
        let guard = Self {
            process: common::ServerProcess::from_child(child, socket.clone()),
            socket,
            _dir: dir,
        };
        let deadline = Instant::now() + SOCKET_DEADLINE;
        while Instant::now() < deadline {
            if guard.socket.exists() {
                return guard;
            }
            std::thread::sleep(POLL);
        }
        panic!(
            "phux server did not bind {} within {SOCKET_DEADLINE:?}",
            guard.socket.display()
        );
    }

    /// `phux <verb> --socket <sock> <rest...>` inside `iso`. `--socket`
    /// is injected right after the verb, NOT appended, because
    /// `run`/`wait`/`send-keys` use `trailing_var_arg` (see `run_wait_e2e`).
    fn cmd(&self, iso: &Isolation, args: &[&str]) -> Command {
        let (verb, rest) = args.split_first().expect("at least a verb");
        let mut cmd = Command::new(PHUX);
        cmd.arg(verb)
            .arg("--socket")
            .arg(&self.socket)
            .args(rest)
            .stdin(Stdio::null());
        iso.apply(&mut cmd);
        cmd
    }

    /// SIGKILL the server NOW (scenario 5's crash injection). `Child::kill`
    /// is SIGKILL on unix — no shutdown handler runs, and the socket file
    /// is left behind, exactly like a real crash.
    fn sigkill(&mut self) {
        self.process.sigkill();
    }
}

/// Does this `phux ls --json` session entry say a client is attached to the
/// scenario session? The counter is the server's own view, incremented when
/// it processes `ATTACH` (see `AttachedClient::wait_until_painting`).
fn is_attached_work_session(session: &serde_json::Value) -> bool {
    session["name"] == SESSION
        && session["attached_clients"]
            .as_u64()
            .is_some_and(|count| count > 0)
}

/// Run a command to completion, returning `(exit_code, stdout, stderr)`.
fn run_captured(cmd: &mut Command) -> (i32, String, String) {
    let out = cmd.output().expect("run phux command");
    let code = out
        .status
        .code()
        .unwrap_or_else(|| panic!("phux terminated by signal: {:?}", out.status));
    (
        code,
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A real `phux attach` TUI on a pseudo-terminal, with everything it paints
/// (stdout AND stderr — a PTY merges them) captured for assertions. Killed
/// on drop so a failing assertion never leaks a client.
struct AttachedClient {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    output: Arc<Mutex<Vec<u8>>>,
}

impl Drop for AttachedClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl AttachedClient {
    fn start(server: &ServerGuard, iso: &Isolation) -> Self {
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows: 24,
                cols: 100,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("open attach PTY");
        let mut command = CommandBuilder::new(PHUX);
        command.args([
            "attach",
            "--socket",
            server.socket.to_str().expect("UTF-8 socket path"),
            SESSION,
        ]);
        command.env("SHELL", "/bin/sh");
        command.env("TERM", "xterm-256color");
        command.env("RUST_LOG", "off");
        command.env("XDG_CONFIG_HOME", iso.config.path());
        command.env("XDG_STATE_HOME", iso.state.path());
        let child = pair
            .slave
            .spawn_command(command)
            .expect("spawn attached TUI");
        drop(pair.slave);

        // Capture (rather than discard) every byte the client emits: the
        // cooked-terminal teardown lines the scenarios assert on arrive on
        // this same stream. Continuous draining also keeps the PTY from
        // backpressuring the client.
        let output = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&output);
        let mut reader = pair.master.try_clone_reader().expect("clone PTY reader");
        std::thread::spawn(move || {
            let mut bytes = [0u8; 8192];
            while let Ok(read) = reader.read(&mut bytes) {
                if read == 0 {
                    break;
                }
                sink.lock()
                    .expect("output lock")
                    .extend_from_slice(&bytes[..read]);
            }
        });
        let writer = pair.master.take_writer().expect("take PTY writer");
        Self {
            child,
            writer,
            output,
        }
    }

    /// Block until this client is a registered observer of `SESSION`.
    ///
    /// This used to be "wait for one painted byte, then sleep 500ms", and the
    /// sleep was the load-dependent seam (phux-hhn2). A painted byte proves
    /// only that `phux attach` reached its terminal setup — it is emitted
    /// before the socket is even dialled — so the 500ms was the entire barrier,
    /// and it is a bet rather than a fact. When it lost, the scenario's
    /// stimulus (a `send-keys` that kills the last pane) landed on a server
    /// this client had not attached to yet, and the failure said nothing at all
    /// about the behavior under test.
    ///
    /// The replacement is a real barrier, on the server's side of the wire:
    /// `phux ls --json` reports `attached_clients`, and that counter only
    /// increments once the server has processed this connection's `ATTACH`.
    /// `ls` is a command that carries a reply, so its exit is proof of
    /// delivery, not merely of sending — the distinction the phux-5wxp
    /// taxonomy is built around. Once it reads non-zero, every later frame
    /// this session emits has somewhere to go.
    ///
    /// Polling by connecting is safe here (it is not the shape-4 hazard):
    /// nothing in these scenarios measures an idle lifetime, and the harness
    /// server's `--exit-after-idle` is ten minutes.
    fn wait_until_attached(&mut self, server: &ServerGuard, iso: &Isolation) {
        let deadline = Instant::now() + CLIENT_DEADLINE;
        while Instant::now() < deadline {
            assert!(
                self.child.try_wait().expect("client try_wait").is_none(),
                "attach client exited before the scenario ran; output:\n{}",
                self.output_text(),
            );
            let (code, stdout, _stderr) = run_captured(&mut server.cmd(iso, &["ls", "--json"]));
            if code == 0
                && let Ok(doc) = serde_json::from_str::<serde_json::Value>(&stdout)
                && doc["sessions"]
                    .as_array()
                    .is_some_and(|sessions| sessions.iter().any(is_attached_work_session))
            {
                return;
            }
            std::thread::sleep(POLL);
        }
        panic!(
            "attach client never registered as attached to {SESSION} within \
             {CLIENT_DEADLINE:?}; output:\n{}",
            self.output_text(),
        );
    }

    /// The residual pause the two keystroke-injecting scenarios still need,
    /// kept separate from [`Self::wait_until_attached`] so it is obvious what
    /// it does and does not cover.
    ///
    /// [`Self::wait_until_attached`] proves the SERVER has this client
    /// registered. It says nothing about the client's own stdin reader, and a
    /// keystroke written into the attach PTY before that reader exists is
    /// simply dropped. There is no observable barrier for a client-local
    /// install — nothing crosses the wire — so this stays a pause, and it is
    /// named for what it is rather than hidden inside a wait helper.
    ///
    /// Scenarios that drive the pane through the SERVER (`phux send-keys`) do
    /// not need this and must not call it: their stimulus is ordered behind a
    /// command the server replies to.
    fn settle_for_local_keystrokes() {
        std::thread::sleep(Duration::from_millis(500));
    }

    fn send(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).expect("write to attach PTY");
        self.writer.flush().expect("flush attach PTY");
    }

    /// Everything captured so far, lossily decoded (the stream carries VT
    /// escapes; the asserted teardown lines are printed on the cooked
    /// screen as plain contiguous text).
    fn output_text(&self) -> String {
        String::from_utf8_lossy(&self.output.lock().expect("output lock")).into_owned()
    }

    /// Wait until the captured output contains `needle`.
    fn wait_for_output(&self, needle: &str) {
        let deadline = Instant::now() + CLIENT_DEADLINE;
        while Instant::now() < deadline {
            if self.output_text().contains(needle) {
                return;
            }
            std::thread::sleep(POLL);
        }
        panic!(
            "attach client never printed {needle:?} within {CLIENT_DEADLINE:?}; output:\n{}",
            self.output_text(),
        );
    }

    /// Wait for the client process to exit, returning its status.
    fn wait_exit(&mut self) -> portable_pty::ExitStatus {
        let deadline = Instant::now() + CLIENT_DEADLINE;
        while Instant::now() < deadline {
            if let Some(status) = self.child.try_wait().expect("client try_wait") {
                return status;
            }
            std::thread::sleep(POLL);
        }
        panic!(
            "attach client did not exit within {CLIENT_DEADLINE:?}; output:\n{}",
            self.output_text(),
        );
    }
}

// ---------------------------------------------------------------------------
// 1. broken config -> loud server start (audited: server.rs:113-148)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "spawns real phux processes; starves in the full parallel pool. Run via `just e2e`."]
fn broken_config_makes_server_start_loud() {
    let iso = Isolation::new();
    let config_path = iso.write_config("defaults = [ this is not toml\n");

    let dir = tempfile::tempdir().expect("socket tempdir");
    let socket = dir.path().join("fx-broken.sock");
    let mut cmd = Command::new(PHUX);
    cmd.args(["server", "--session", SESSION, "--socket"])
        .arg(&socket)
        .args(["--exit-after-idle", "30"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    iso.apply(&mut cmd);
    // Owned by a guard, not a bare `Child` (phux-whhd). The assertion below
    // says "killing it" on the regression path, and nothing was: this test
    // held a plain `std::process::Child`, whose `Drop` does NOT kill. If the
    // audited regression ever came back — a server that starts normally on a
    // broken config — the very run that caught it would have leaked the daemon
    // it caught. `ServerProcess::drop` SIGKILLs a survivor and unlinks the
    // socket, so the failing path cleans up after itself.
    let mut server =
        common::ServerProcess::from_child(cmd.spawn().expect("spawn phux server"), socket);

    // Poll rather than block on `.output()`: the audited regression is a
    // server that starts NORMALLY on a broken config, and that regression
    // would hang a blocking wait for the whole idle backstop.
    let deadline = Instant::now() + SOCKET_DEADLINE;
    let status = loop {
        if let Some(status) = server.child_mut().try_wait().expect("server try_wait") {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "server with a broken config did not exit within {SOCKET_DEADLINE:?} \
             (the audited silent-start regression)",
        );
        std::thread::sleep(POLL);
    };
    let mut stderr = String::new();
    server
        .child_mut()
        .stderr
        .take()
        .expect("piped stderr")
        .read_to_string(&mut stderr)
        .expect("read server stderr");

    assert!(
        !status.success(),
        "a broken config must refuse the start; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("cannot start"),
        "the refusal must be loud on stderr:\n{stderr}"
    );
    assert!(
        stderr.contains(&config_path.display().to_string()),
        "the refusal must name the config path {}:\n{stderr}",
        config_path.display(),
    );
    assert!(
        stderr.contains("phux config check"),
        "the refusal must name its remedy `phux config check`:\n{stderr}"
    );
}

// ---------------------------------------------------------------------------
// 2. typo'd action named at check (audited: input_dispatch.rs:2560)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "spawns real phux processes; starves in the full parallel pool. Run via `just e2e`."]
fn config_check_names_the_typoed_action() {
    let iso = Isolation::new();
    let config_path = iso.write_config("[keybindings.prefix-table]\nq = \"kill-pain\"\n");

    let mut cmd = Command::new(PHUX);
    cmd.args(["config", "check"])
        .arg(&config_path)
        .stdin(Stdio::null());
    iso.apply(&mut cmd);
    let (code, stdout, _stderr) = run_captured(&mut cmd);

    assert_eq!(code, 1, "a semantic finding must exit 1; stdout:\n{stdout}");
    assert!(
        stdout.contains("keybindings.prefix-table.q"),
        "check must name the offending binding:\n{stdout}"
    );
    assert!(
        stdout.contains("unknown name"),
        "check must carry the fault label:\n{stdout}"
    );
    assert!(
        stdout.contains("did you mean `kill-pane`?"),
        "check must suggest the intended action:\n{stdout}"
    );
}

// ---------------------------------------------------------------------------
// 3. malformed chord does not kill keybindings/detach (audited: driver.rs:3709)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "spawns real phux processes; starves in the full parallel pool. Run via `just e2e`."]
fn malformed_chord_keeps_detach_alive() {
    // Valid TOML, one malformed chord key ("q-": trailing dash). Before
    // phux-i0e8.3.4 this made the resolver build fail closed: EVERY
    // binding died, including detach, and the only way out was kill -9.
    let iso = Isolation::new();
    iso.write_config("[keybindings.prefix-table]\n\"q-\" = \"kill-pane\"\nd = \"detach\"\n");
    let server = ServerGuard::start(&iso);

    let mut client = AttachedClient::start(&server, &iso);
    client.wait_until_attached(&server, &iso);
    AttachedClient::settle_for_local_keystrokes();

    // The default prefix (C-a) then `d`: detach must still resolve.
    client.send(b"\x01d");
    let status = client.wait_exit();
    assert!(
        status.success(),
        "detach must survive one malformed chord (exit {:?}); output:\n{}",
        status.exit_code(),
        client.output_text(),
    );
}

// ---------------------------------------------------------------------------
// 4. pane death surfaces exit status (audited: server_frame.rs:1169-1216)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "spawns real phux processes; starves in the full parallel pool. Run via `just e2e`."]
fn last_pane_death_surfaces_its_exit_status() {
    let iso = Isolation::new();
    let server = ServerGuard::start(&iso);

    let mut client = AttachedClient::start(&server, &iso);
    client.wait_until_attached(&server, &iso);

    // Kill the seed pane's shell with a distinctive status. The exit code
    // must ride RESOURCE_CLOSED to the client and come out in the
    // teardown line — not be discarded as it was when audited.
    let (code, _stdout, stderr) =
        run_captured(&mut server.cmd(&iso, &["send-keys", SESSION, "exit 7", "Enter"]));
    assert_eq!(code, 0, "send-keys must succeed; stderr:\n{stderr}");

    let status = client.wait_exit();
    assert!(
        status.success(),
        "a last-pane death is an explained ending, not a client failure; output:\n{}",
        client.output_text(),
    );
    client.wait_for_output("the last pane exited 7");
}

// ---------------------------------------------------------------------------
// 5. server SIGKILL shows the reconnect indicator (audited: attach.rs:272-341)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "spawns real phux processes; starves in the full parallel pool. Run via `just e2e`."]
fn server_sigkill_shows_the_reconnect_indicator() {
    // Retry-tolerant by lane design: `just e2e` runs this serially with
    // --retries=2. If it flakes anyway, demote ONLY this test to the
    // `stress` lane (see the module doc).
    let iso = Isolation::new();
    let mut server = ServerGuard::start(&iso);

    let mut client = AttachedClient::start(&server, &iso);
    client.wait_until_attached(&server, &iso);

    // Crash the server for real. SIGKILL runs no shutdown handler and
    // leaves the socket file behind — the audited case that used to be
    // ~10s of blank screen.
    server.sigkill();

    // The client must drop to the cooked screen and SAY what happened,
    // starting its visible countdown. Asserting the first line (not the
    // countdown repaints or the timeout report) keeps this independent of
    // the 10s reconnect window's outcome.
    client.wait_for_output("lost the server connection");
}

// ---------------------------------------------------------------------------
// 6. dead-socket --json parses as the contract (ADR-0065 section 4)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "spawns real phux processes; starves in the full parallel pool. Run via `just e2e`."]
fn dead_socket_json_error_is_the_contract() {
    let iso = Isolation::new();
    let dir = tempfile::tempdir().expect("socket tempdir");
    let socket = dir.path().join("fx-absent.sock");

    let mut cmd = Command::new(PHUX);
    cmd.arg("ls")
        .arg("--socket")
        .arg(&socket)
        .arg("--json")
        .stdin(Stdio::null());
    iso.apply(&mut cmd);
    let (code, stdout, stderr) = run_captured(&mut cmd);

    assert_eq!(code, 1, "no server is exit 1; stderr:\n{stderr}");
    assert!(
        stdout.is_empty(),
        "stdout is the document and stays empty on failure:\n{stdout}"
    );
    let line = stderr.trim();
    assert!(
        !line.contains('\n'),
        "the error is ONE line of JSON on stderr:\n{stderr}"
    );
    let doc: serde_json::Value =
        serde_json::from_str(line).expect("dead-socket --json stderr parses as JSON");
    assert_eq!(doc["schema_version"], 1, "contract schema version:\n{doc}");
    assert_eq!(
        doc["error"]["code"], "no_server",
        "closed vocabulary:\n{doc}"
    );
    assert!(
        doc["error"]["message"]
            .as_str()
            .expect("message is a string")
            .contains(&socket.display().to_string()),
        "the message names the socket:\n{doc}"
    );
    assert!(
        !doc["remedy"]
            .as_str()
            .expect("remedy is a string")
            .is_empty(),
        "every failure names its remedy:\n{doc}"
    );
    assert_eq!(doc["exit_code"], 1, "embedded exit code matches:\n{doc}");
}

// ---------------------------------------------------------------------------
// 7. status/logs/doctor speak with real paths (audit pattern 2: INVISIBILITY)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "spawns real phux processes; starves in the full parallel pool. Run via `just e2e`."]
fn status_logs_doctor_name_real_paths() {
    let iso = Isolation::new();
    let server = ServerGuard::start(&iso);
    let server_log = iso.server_log().display().to_string();

    // `phux status` against the live server names the canonical log paths.
    let (code, stdout, stderr) = run_captured(&mut server.cmd(&iso, &["status"]));
    assert_eq!(
        code, 0,
        "status against a live server exits 0; stderr:\n{stderr}"
    );
    assert!(
        stdout.contains(&server_log),
        "status must name the server log {server_log}:\n{stdout}"
    );

    // Bare `phux logs` prints the inventory — every path, no server needed.
    let mut logs_cmd = Command::new(PHUX);
    logs_cmd.arg("logs").stdin(Stdio::null());
    iso.apply(&mut logs_cmd);
    let (code, stdout, stderr) = run_captured(&mut logs_cmd);
    assert_eq!(code, 0, "the logs inventory exits 0; stderr:\n{stderr}");
    assert!(
        stdout.contains(&server_log),
        "logs must name the server log {server_log}:\n{stdout}"
    );

    // `phux doctor` composes the checks and names the same paths; warnings
    // (for example a log not created yet) are normal states, not failures.
    let (code, stdout, stderr) = run_captured(&mut server.cmd(&iso, &["doctor"]));
    assert_eq!(
        code, 0,
        "doctor with a healthy setup exits 0; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains(&server.socket.display().to_string()),
        "doctor must name the socket it probed:\n{stdout}"
    );
    assert!(
        stdout.contains(&server_log),
        "doctor must name the server log {server_log}:\n{stdout}"
    );
}
