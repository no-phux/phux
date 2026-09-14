//! `phux host add HOST` against a real server, end to end (ADR-0122).
//!
//! The fleet tests prove every ssh step and every fallback with nothing
//! answering. This file proves the path that matters most: a direct route
//! that does answer. A real `phux server` runs here with a loopback QUIC
//! listener, standing in for the far machine; a fake `ssh` via `$PHUX_SSH`
//! runs the remote half — `phux --version`, `phux pair --json`, `phux
//! server --ensure` — on this machine with the same private state, the way
//! sshd would run it on the far end, and answers `ssh -G` with `127.0.0.1`.
//! The probe therefore dials the real listener with the credentials the
//! real `phux pair` just minted, and the entry it registers is the one the
//! session verbs and the attach then use.
//!
//! Hermetic: private config, state, and runtime dirs, no overlay detection,
//! and a socket inside the tempdir so the developer's own server is never
//! touched. `PHUX_WS_SECURE=1` makes the loopback listener take the routable
//! path (certificate pin plus bearer token), which is what the probe and the
//! registered entry exercise on a real network.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use tempfile::TempDir;

const PHUX: &str = env!("CARGO_BIN_EXE_phux");

/// How long the freshly started server has to bind its socket.
const READY_DEADLINE: Duration = Duration::from_secs(30);

/// How long a PTY-backed run has to reach the line a test waits for.
const DEADLINE: Duration = Duration::from_secs(60);

/// How long to keep reading after that line, so what follows is captured.
const SETTLE: Duration = Duration::from_millis(750);

/// A real server behind a loopback QUIC listener, plus the fake ssh that
/// makes it look like a machine called `box`.
struct FarHost {
    dir: TempDir,
    port: u16,
    server: Option<Child>,
}

impl FarHost {
    fn start() -> Self {
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path();
        for sub in ["config/phux", "state", "run", "bin"] {
            std::fs::create_dir_all(root.join(sub)).expect("scratch dirs");
        }
        std::os::unix::fs::symlink(PHUX, root.join("bin/phux")).expect("phux on the fake PATH");
        let port = free_udp_port();

        // The fake remote shell: `-G` names loopback; a remote command runs
        // this build with the far host's environment. `service install`
        // is answered by starting (or re-starting) the server the way the
        // unit would, since no init system is in the loop here.
        let env_exports =
            hermetic_env(root)
                .into_iter()
                .fold(String::new(), |mut exports, (key, value)| {
                    use std::fmt::Write as _;
                    let _ = writeln!(
                        exports,
                        "export {key}={}",
                        shell_quote(&value.display().to_string())
                    );
                    exports
                });
        let script = format!(
            "#!/bin/sh\n\
             printf '%s\\n' \"$*\" >> {calls}\n\
             if [ \"$1\" = \"-G\" ]; then echo 'user me'; echo 'hostname 127.0.0.1'; exit 0; fi\n\
             {env_exports}\
             export PATH={bin}:$PATH\n\
             export PHUX_QUIC_ADDR=127.0.0.1:{port}\n\
             shift 3\n\
             case \"$*\" in\n\
               \"phux service install\"*) exec phux server --ensure ;;\n\
               *) exec \"$@\" ;;\n\
             esac\n",
            calls = shell_quote(&root.join("ssh-calls").display().to_string()),
            bin = shell_quote(&root.join("bin").display().to_string()),
        );
        let ssh = root.join("fake-ssh");
        std::fs::write(&ssh, script).expect("write fake ssh");
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755))
                .expect("chmod fake ssh");
        }

        let mut host = Self {
            dir,
            port,
            server: None,
        };
        host.start_server();
        host
    }

    /// Start the far host's server with its QUIC listener, as its service
    /// unit would.
    fn start_server(&mut self) {
        let server = Command::new(PHUX)
            .envs(hermetic_env(self.dir.path()))
            .arg("server")
            .arg("--socket")
            .arg(self.socket())
            .args(["--quic", &format!("127.0.0.1:{}", self.port)])
            // Backstop only: Drop kills the server, but a panic that skips
            // it must not leave a daemon behind for long.
            .args(["--exit-after-idle", "120"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn phux server");
        self.server = Some(server);
        let start = Instant::now();
        while !self.socket().exists() {
            assert!(
                start.elapsed() < READY_DEADLINE,
                "the far host's server never bound its socket"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Stop the far host's server the way an operator's `kill` would: a
    /// clean exit that no supervisor restarts.
    fn stop_server(&mut self) {
        let out = self.phux(&["kill", "--server"]);
        assert!(out.status.success(), "kill --server: {}", stderr(&out));
        if let Some(mut server) = self.server.take() {
            let _ = server.wait();
        }
        let start = Instant::now();
        while self.socket().exists() {
            assert!(
                start.elapsed() < READY_DEADLINE,
                "the far host's socket never went away"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn socket(&self) -> PathBuf {
        self.dir.path().join("run/phux.sock")
    }

    fn env(&self) -> Vec<(&'static str, PathBuf)> {
        let mut env = hermetic_env(self.dir.path());
        env.push(("PHUX_SSH", self.dir.path().join("fake-ssh")));
        env
    }

    /// Run `phux <args...>` as the laptop side: same private dirs, the fake
    /// ssh, and no socket handed over, so the registry is the only way to
    /// the server.
    fn phux(&self, args: &[&str]) -> Output {
        Command::new(PHUX)
            .envs(self.env())
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("run phux")
    }

    fn config(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("config/phux/config.toml")).unwrap_or_default()
    }

    fn ssh_calls(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("ssh-calls")).unwrap_or_default()
    }

    /// Run `phux ARGS` on a PTY until `needle` appears or [`DEADLINE`]
    /// passes, then kill it. The attach's TTY preflight needs the PTY.
    fn run_until(&self, args: &[&str], needle: &str) -> String {
        let pty = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        let mut cmd = CommandBuilder::new(PHUX);
        cmd.args(args);
        cmd.env_clear();
        if let Some(path) = std::env::var_os("PATH") {
            cmd.env("PATH", path);
        }
        if let Some(tmp) = std::env::var_os("TMPDIR") {
            cmd.env("TMPDIR", tmp);
        }
        for (key, value) in self.env() {
            cmd.env(key, value);
        }
        cmd.env("TERM", "xterm-256color");
        let mut child = pty.slave.spawn_command(cmd).expect("spawn phux");
        drop(pty.slave);
        let mut reader = pty.master.try_clone_reader().expect("clone reader");

        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut buf = [0_u8; 4096];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });

        let start = Instant::now();
        let mut seen = String::new();
        let mut settle_until = None;
        loop {
            let budget = settle_until.map_or_else(
                || DEADLINE.saturating_sub(start.elapsed()),
                |until: Instant| until.saturating_duration_since(Instant::now()),
            );
            if budget.is_zero() {
                break;
            }
            match rx.recv_timeout(budget) {
                Ok(chunk) => {
                    seen.push_str(&String::from_utf8_lossy(&chunk));
                    if settle_until.is_none() && seen.contains(needle) {
                        settle_until = Some(Instant::now() + SETTLE);
                    }
                }
                Err(_) => break,
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        seen
    }
}

impl Drop for FarHost {
    fn drop(&mut self) {
        // The repair rung may have started a server of its own through the
        // fake ssh; stop whatever holds the socket, then the child.
        let _ = Command::new(PHUX)
            .envs(self.env())
            .args(["kill", "--server"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if let Some(mut server) = self.server.take() {
            let _ = server.kill();
            let _ = server.wait();
        }
    }
}

/// The environment every process in this test runs under: the far host's
/// server and the laptop side share one private state dir, which is what
/// running the remote half on this machine means.
fn hermetic_env(dir: &Path) -> Vec<(&'static str, PathBuf)> {
    vec![
        ("HOME", dir.to_path_buf()),
        ("XDG_CONFIG_HOME", dir.join("config")),
        ("XDG_STATE_HOME", dir.join("state")),
        ("XDG_RUNTIME_DIR", dir.join("run")),
        ("PHUX_SOCKET", dir.join("run/phux.sock")),
        ("PHUX_PROFILE", PathBuf::from("default")),
        ("PHUX_NO_AUTO_LISTEN", PathBuf::from("1")),
        ("PHUX_TAILSCALE", dir.join("no-such-tailscale")),
        // A loopback listener expects no token preamble; the far host is
        // only on loopback here, so force the routable path (TLS pin plus
        // bearer token) the real one takes.
        ("PHUX_WS_SECURE", PathBuf::from("1")),
    ]
}

fn shell_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A UDP port nothing is bound to right now.
fn free_udp_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .expect("bind a free udp port")
        .local_addr()
        .expect("local addr")
        .port()
}

/// The whole story on one host: `host add` finds phux, pairs, dials the
/// route `ssh -G` named, and registers it direct; the session verbs reach
/// the server through the entry with no ssh; a second `host add` finds the
/// route answering and touches nothing; and after the server is stopped by
/// hand, `phux attach NAME` starts it over ssh and attaches.
#[test]
#[ignore = "spawns a real server and a PTY-backed binary; runs in the e2e lane"]
fn host_add_registers_a_direct_route_and_attach_restarts_a_stopped_server() {
    let mut host = FarHost::start();
    let port = host.port.to_string();

    // 1. Add the machine. The far host's `phux pair --json` is the real one,
    //    against the real listener's token store; the probe dials it.
    let out = host.phux(&["host", "add", "me@box", "--quic-port", &port]);
    let (stdout, err) = (String::from_utf8_lossy(&out.stdout), stderr(&out));
    assert!(
        out.status.success(),
        "host add: stdout={stdout} stderr={err}"
    );
    let endpoint = format!("quic://127.0.0.1:{port}");
    assert!(
        stdout.contains(&format!("Registered box -> {endpoint}"))
            && stdout.contains("phux attach box"),
        "stdout={stdout}\nstderr={err}"
    );
    assert!(
        err.contains("box: paired")
            && err.contains(&format!("direct route reachable at {endpoint}")),
        "stderr={err}"
    );
    let config = host.config();
    assert!(
        config.contains(&format!("endpoint = \"{endpoint}\""))
            && config.contains("ssh = \"me@box\"")
            && !config.contains("direct ="),
        "a route that answered is the endpoint, with nothing left to promote; config={config}"
    );
    assert!(
        host.dir
            .path()
            .join("state/phux/remotes/box.token")
            .exists(),
        "the minted token is stored for the dial"
    );
    let calls = host.ssh_calls();
    assert!(
        calls.contains("phux --version")
            && calls.contains("phux service install")
            && calls.contains("phux pair --json")
            && calls.contains("-G -- me@box"),
        "calls={calls:?}"
    );

    // 2. The entry is enough for the session verbs: no ssh in the path.
    let before = host.ssh_calls().lines().count();
    let out = host.phux(&["ls", "--remote", "box", "--json"]);
    assert!(out.status.success(), "ls --remote box: {}", stderr(&out));
    assert_eq!(
        host.ssh_calls().lines().count(),
        before,
        "a registered direct route never touches ssh"
    );

    // 3. Adding it again finds it answering and re-pairs nothing.
    let out = host.phux(&["host", "add", "me@box", "--quic-port", &port]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "re-add: {}", stderr(&out));
    assert!(
        stdout.contains("already registered and answering"),
        "stdout={stdout}"
    );
    assert_eq!(
        host.ssh_calls().lines().count(),
        before,
        "an answering host is not set up again"
    );

    // 4. Stop the server by hand. The attach finds nobody answering, starts
    //    it over ssh, and attaches with the credentials it already has.
    host.stop_server();
    let seen = host.run_until(&["attach", "box"], "\u{1b}[?1049h");
    assert!(
        seen.contains(&format!("box is not answering at {endpoint}"))
            && seen.contains("starting its server over ssh (me@box)"),
        "the attach must start the stopped server rather than fail; got: {seen}"
    );
    assert!(
        !seen.contains("re-pairing"),
        "the saved credentials still work, so nothing is re-paired; got: {seen}"
    );
    assert!(
        seen.contains("\u{1b}[?1049h"),
        "the TUI came up over the direct route after the restart: {seen}"
    );
    let calls = host.ssh_calls();
    assert_eq!(
        calls.matches("phux pair --json").count(),
        1,
        "pairing happened exactly once, at the add; calls={calls:?}"
    );
}
