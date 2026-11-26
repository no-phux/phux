//! `host add` onto a machine whose phux already runs, started by hand with
//! no `[policy]` and no listener: the running server opens one with its panes
//! kept, or nothing there changes. The far machine is this one under its own
//! `HOME`, behind a fake `ssh`; its init tools are fakes that run nothing.

use std::net::UdpSocket;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use crate::common;

/// What the far machine's owner wrote before any of this.
const CONFIG: &str = "# mine\n[sidebar]\nwidth = 30\n";

struct Machines {
    dir: tempfile::TempDir,
    server: common::AutoSpawnedServer,
    pane: u32,
}

impl Machines {
    /// A far machine whose phux was started by `phux new`, with one pane.
    fn start() -> Self {
        Self::start_with(&[])
    }

    /// [`Self::start`], the far server under `extra` environment.
    fn start_with(extra: &[(&str, &str)]) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("root");
        for sub in ["far/config/phux", "far/run", "bin", "near/config/phux"] {
            std::fs::create_dir_all(root.join(sub)).expect("scratch dirs");
        }
        for tool in ["launchctl", "systemctl", "loginctl"] {
            // Nothing is running or lingering; every write is accepted.
            common::write_executable(
                root.join("bin").join(tool),
                "#!/bin/sh\ncase \"$*\" in print*|*status*|*show*) exit 3 ;; esac\nexit 0\n",
            );
        }
        std::os::unix::fs::symlink(crate::runner::phux_bin(), root.join("bin/phux"))
            .expect("phux on the far PATH");
        let far = env(&root.join("far"));
        let exports = far
            .iter()
            .map(|(key, value)| format!("export {key}='{}'", value.display()))
            .collect::<Vec<_>>()
            .join("\n");
        // The far bind moves to loopback; nothing else about it changes.
        let script = format!(
            "#!/bin/sh\n\
             if [ \"$1\" = \"-G\" ]; then echo 'user me'; echo 'hostname 127.0.0.1'; exit 0; fi\n\
             {exports}\n\
             export PATH='{bin}':/usr/bin:/bin\n\
             while [ \"$1\" = -o ]; do shift 2; done\n\
             shift\n\
             exec /bin/sh -c \"$(printf %s \"$*\" | sed 's/0\\.0\\.0\\.0/127.0.0.1/')\"\n",
            bin = root.join("bin").display(),
        );
        common::write_executable(root.join("fake-ssh"), &script);
        let server =
            common::AutoSpawnedServer::new(crate::runner::phux_bin(), root.join("far/run/s.sock"));
        let mut machines = Self {
            dir,
            server,
            pane: 0,
        };
        let out = machines
            .command(
                "far",
                &["new", "-s", "work", "--json", "--", "sleep", "600"],
            )
            .envs(extra.iter().copied())
            .output()
            .expect("run phux");
        assert!(out.status.success(), "{}", super::stderr(&out));
        machines.pane = common::child_of(machines.server.capture_pid()).expect("pane");
        machines
    }

    fn root(&self) -> PathBuf {
        self.dir.path().canonicalize().expect("root")
    }

    /// Run `phux ARGS` on the `far` or the `near` machine.
    fn run(&self, machine: &str, args: &[&str]) -> Output {
        self.command(machine, args).output().expect("run phux")
    }

    fn command(&self, machine: &str, args: &[&str]) -> Command {
        let root = self.root();
        let mut command = common::phux_cmd(crate::runner::phux_bin());
        command
            .envs(env(&root.join(machine)))
            .env("PHUX_SSH", root.join("fake-ssh"))
            .env("PHUX_DIRECT_PROBE_TIMEOUT_MS", "3000")
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", root.join("bin").display()),
            )
            .args(args)
            .stdin(Stdio::null());
        command
    }

    fn unit(&self) -> PathBuf {
        let home = self.root().join("far");
        if cfg!(target_os = "macos") {
            home.join("Library/LaunchAgents/com.phux.server.plist")
        } else {
            home.join("config/systemd/user/phux.service")
        }
    }

    fn far_config_path(&self) -> PathBuf {
        self.root().join("far/config/phux/config.toml")
    }

    fn far_config(&self) -> Option<String> {
        std::fs::read_to_string(self.far_config_path()).ok()
    }

    /// The pane runs on, and the config and unit are as before the add.
    fn assert_untouched(&self) {
        assert!(common::process_exists(self.pane), "the pane survives");
        assert!(!self.unit().exists(), "no unit is left behind");
        assert_eq!(self.far_config().as_deref(), Some(CONFIG));
        assert!(
            !self
                .root()
                .join("far/state/phux/service-adopt-pending")
                .exists()
        );
    }
}

fn env(home: &std::path::Path) -> Vec<(&'static str, PathBuf)> {
    vec![
        ("HOME", home.to_path_buf()),
        ("XDG_CONFIG_HOME", home.join("config")),
        ("XDG_STATE_HOME", home.join("state")),
        ("XDG_RUNTIME_DIR", home.join("run")),
        ("PHUX_SOCKET", home.join("run/s.sock")),
        ("PHUX_PROFILE", PathBuf::from("default")),
        ("PHUX_NO_AUTO_LISTEN", PathBuf::from("1")),
        ("PHUX_TAILSCALE", home.join("no-such-tailscale")),
        ("PHUX_WS_SECURE", PathBuf::from("1")),
        ("SHELL", PathBuf::from("/bin/sh")),
    ]
}

#[test]
#[ignore = "spawns and re-execs a real server; runs in the e2e lane"]
fn host_add_opens_a_listener_on_a_server_already_running_there() {
    let machines = Machines::start();
    let port = UdpSocket::bind("127.0.0.1:0")
        .and_then(|socket| socket.local_addr())
        .expect("free port")
        .port()
        .to_string();

    let out = machines.run("near", &["host", "add", "me@box", "--quic-port", &port]);
    let (stdout, err) = (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        super::stderr(&out),
    );
    assert!(out.status.success(), "stdout={stdout} stderr={err}");
    assert!(
        stdout.contains(&format!("Registered box -> quic://127.0.0.1:{port}")),
        "stdout={stdout} stderr={err}"
    );
    assert!(common::process_exists(machines.pane), "the pane survives");
    assert!(machines.unit().exists(), "supervision is armed");
    assert!(
        machines
            .far_config()
            .is_some_and(|config| config.contains("mode = \"paired\""))
    );
    let out = machines.run("near", &["ls", "--remote", "box"]);
    assert!(
        out.status.success() && String::from_utf8_lossy(&out.stdout).contains("work"),
        "{out:?}"
    );
}

#[test]
#[ignore = "spawns a real server; runs in the e2e lane"]
fn host_add_with_the_port_taken_changes_nothing_there() {
    let machines = Machines::start();
    let held = UdpSocket::bind("127.0.0.1:0").expect("hold a port");
    let port = held.local_addr().expect("addr").port().to_string();
    let config = machines.far_config_path();
    std::fs::write(&config, CONFIG).expect("config");

    for _ in 0..2 {
        // Nothing half applied to trip over the second time.
        let out = machines.run("near", &["host", "add", "me@box", "--quic-port", &port]);
        let err = super::stderr(&out);
        assert!(!out.status.success());
        assert!(
            err.contains("accepts no network connections")
                && err.contains(&format!("already uses UDP port {port}"))
                && err.contains("--quic-port PORT"),
            "{err}"
        );
        assert!(!err.contains("pass `--listen"), "{err}");
        machines.assert_untouched();
    }
}

/// The server reloads but cannot open the listener (its certificate is
/// unreadable), so the mode and the unit it was given are taken back.
#[test]
#[ignore = "spawns and re-execs a real server; runs in the e2e lane"]
fn host_add_takes_back_a_setup_whose_listener_does_not_open() {
    let missing = "/nonexistent/phux-test.pem";
    let machines =
        Machines::start_with(&[("PHUX_WS_TLS_CERT", missing), ("PHUX_WS_TLS_KEY", missing)]);
    std::fs::write(machines.far_config_path(), CONFIG).expect("config");

    let out = machines.run("near", &["host", "add", "me@box", "--quic-port", "18788"]);
    let err = super::stderr(&out);
    assert!(!out.status.success());
    assert!(
        err.contains("accepts no network connections")
            && err.contains("the running server could not open quic on 127.0.0.1:18788"),
        "{err}"
    );
    machines.assert_untouched();
}
