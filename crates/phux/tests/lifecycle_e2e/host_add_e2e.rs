//! `phux host add HOST` (ADR-0122) where a direct route answers: a real
//! server with a loopback QUIC listener stands in for the far machine, and a
//! fake `ssh` via `$PHUX_SSH` runs the remote half locally with the same
//! state. Hermetic dirs, no overlay detection; `PHUX_WS_SECURE=1` makes the
//! loopback listener take the routable (pinned + token) path.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

// Every process here starts from a scrubbed `PHUX_*` environment: run from
// a phux pane, an inherited `PHUX_WS_TOKENS` / `PHUX_WS_TLS_CERT` would
// point the far host's server and its `phux pair` at the operator's real
// credential store and certificate.
#[path = "../common/ambient.rs"]
mod common;
#[path = "../common/listeners.rs"]
mod listeners;
#[path = "../common/paired.rs"]
mod paired;

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use tempfile::TempDir;

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
        Self::start_with(true)
    }

    /// A far host with no `phux` on its `PATH`, where the official
    /// installer (a stub served by a fake `curl`) links this build into
    /// `~/.local/bin`, as the real one installs a release there.
    fn start_without_phux() -> Self {
        Self::start_with(false)
    }

    fn start_with(phux_on_path: bool) -> Self {
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path();
        for sub in ["config/phux", "state", "run", "bin"] {
            std::fs::create_dir_all(root.join(sub)).expect("scratch dirs");
        }
        if phux_on_path {
            std::os::unix::fs::symlink(crate::runner::phux_bin(), root.join("bin/phux"))
                .expect("phux on the fake PATH");
        } else {
            let installer = format!(
                "mkdir -p \"$HOME/.local/bin\"\n\
                 ln -sf {phux} \"$HOME/.local/bin/phux\"\n\
                 echo \"installed phux with: $*\"\n",
                phux = shell_quote(crate::runner::phux_bin()),
            );
            let curl = format!(
                "#!/bin/sh\n\
                 printf '%s\\n' \"$*\" >> {log}\n\
                 while [ \"$1\" != -o ]; do shift; done\n\
                 cat > \"$2\" <<'INSTALLER'\n{installer}INSTALLER\n",
                log = shell_quote(&root.join("curl-calls").display().to_string()),
            );
            write_executable(&root.join("bin/curl"), &curl);
        }
        // A remote listener runs only under `paired` (ADR-0179); `host add`
        // enrolls the certificate the direct route then presents.
        paired::write_paired_policy(&root.join("config/phux/config.toml"));

        // The first server binds a port the kernel picks; the fake ssh's
        // restart then asks for that same port, as a service unit would.
        let mut host = Self {
            dir,
            port: 0,
            server: None,
        };
        host.start_server();
        host.port = listeners::bound_listener_addr(
            &host.socket(),
            listeners::RemoteListenerTransport::Quic,
        )
        .port();
        let root = host.dir.path();
        let port = host.port;

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
        // Like ssh, the remote command is the arguments joined into one line
        // for the far shell, so quoting survives the way it does for real.
        let path = if phux_on_path {
            "$PATH"
        } else {
            "/usr/bin:/bin"
        };
        let script = format!(
            "#!/bin/sh\n\
             printf '%s\\n' \"$*\" >> {calls}\n\
             if [ \"$1\" = \"-G\" ]; then echo 'user me'; echo 'hostname 127.0.0.1'; exit 0; fi\n\
             {env_exports}\
             export PATH={bin}:{path}\n\
             export PHUX_QUIC_ADDR=127.0.0.1:{port}\n\
             while [ \"$1\" = -o ]; do shift 2; done\n\
             shift\n\
             case \"$*\" in\n\
               *\"phux service install\"*) exec \"$1\" server --ensure ;;\n\
               *) exec /bin/sh -c \"$*\" ;;\n\
             esac\n",
            calls = shell_quote(&root.join("ssh-calls").display().to_string()),
            bin = shell_quote(&root.join("bin").display().to_string()),
        );
        write_executable(&root.join("fake-ssh"), &script);

        host
    }

    /// Start the far host's server with its QUIC listener on a port the
    /// kernel picks.
    fn start_server(&mut self) {
        let server = common::phux_cmd(crate::runner::phux_bin())
            .envs(hermetic_env(self.dir.path()))
            .arg("server")
            .arg("--socket")
            .arg(self.socket())
            .args(["--quic", listeners::LOOPBACK_ANY_PORT])
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
        common::phux_cmd(crate::runner::phux_bin())
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
        let mut cmd = CommandBuilder::new(crate::runner::phux_bin());
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
        cmd.env("PHUX_NO_UPDATE_CHECK", "1");
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
        let _ = common::phux_cmd(crate::runner::phux_bin())
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

fn write_executable(path: &Path, script: &str) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::write(path, script).expect("write script");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod script");
}

fn shell_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
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

/// `--install` on a machine without phux: the official installer runs over
/// ssh (on this build's channel), the enrollment then drives the binary it
/// installed by its absolute path, and `--accept-new-host-key` reaches
/// every ssh run. Without `--install` the same host is refused with a
/// remedy that offers it.
#[test]
#[ignore = "spawns a real server; runs in the e2e lane"]
fn host_add_install_puts_phux_on_a_host_without_it() {
    let host = FarHost::start_without_phux();
    let port = host.port.to_string();

    let out = host.phux(&["host", "add", "me@box", "--quic-port", &port, "--json"]);
    assert!(!out.status.success(), "no phux there, no --install");
    let err = stderr(&out);
    assert!(
        err.contains("\"code\":\"registry\"") && err.contains("phux host add me@box --install"),
        "the refusal offers --install: {err}"
    );

    let out = host.phux(&[
        "host",
        "add",
        "me@box",
        "--quic-port",
        &port,
        "--install",
        "--accept-new-host-key",
    ]);
    let (stdout, err) = (String::from_utf8_lossy(&out.stdout), stderr(&out));
    assert!(
        out.status.success(),
        "host add --install: stdout={stdout} stderr={err}"
    );
    let installed = host.dir.path().join(".local/bin/phux");
    assert!(
        installed.exists(),
        "the installer linked phux into ~/.local/bin"
    );
    assert!(
        err.contains("box: phux is not installed there; installing the latest")
            && err.contains(&format!("at {}", installed.display()))
            && err.contains("box: paired"),
        "stderr={err}"
    );
    assert!(
        stdout.contains(&format!("Registered box -> quic://127.0.0.1:{port}")),
        "stdout={stdout}\nstderr={err}"
    );
    let calls = host.ssh_calls();
    assert!(
        calls.contains(&format!("{} pair --json", installed.display()))
            && calls.contains(&format!("{} service install", installed.display())),
        "enrollment drives the installed binary by its path: {calls}"
    );
    assert!(
        calls.contains("-o BatchMode=yes -o StrictHostKeyChecking=accept-new me@box sh -c ")
            && calls.matches("-o BatchMode=yes me@box").count() == 1,
        "every remote command after the refused first add trusts a first-seen host key: {calls}"
    );
    let curl = std::fs::read_to_string(host.dir.path().join("curl-calls")).unwrap_or_default();
    assert!(curl.contains("https://phux.sh/install"), "curl={curl}");

    // Set up again from scratch: the phux installed before is found where
    // the installer put it, not installed a second time.
    let out = host.phux(&["host", "rm", "box"]);
    assert!(out.status.success(), "host rm: {}", stderr(&out));
    let out = host.phux(&["host", "add", "me@box", "--quic-port", &port, "--install"]);
    let err = stderr(&out);
    assert!(out.status.success(), "second host add --install: {err}");
    assert!(!err.contains("installing the latest"), "stderr={err}");
    let curl = std::fs::read_to_string(host.dir.path().join("curl-calls")).unwrap_or_default();
    assert_eq!(curl.lines().count(), 1, "installed once: {curl}");
}
