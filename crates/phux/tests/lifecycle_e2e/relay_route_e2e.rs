//! A relay route end to end through the real binary (ADR-0149): `phux relay
//! run`, a `phux server` whose only door is its `[[connector]]`, `phux pair
//! --relay-route` minting the link, and clients in their own homes that
//! register through that link (`attach --remote --code`, on a PTY) or through
//! `host add --tls-server-name`, then list sessions over the relay. Every
//! process has a private home; nothing touches the operator's server, config,
//! or relay state.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

#[path = "../common/ambient.rs"]
mod common;

use std::io::{BufRead as _, BufReader, Read as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use tempfile::TempDir;

/// The route the relay enrolls the server's connector under.
const ROUTE: &str = "mini-route";

/// How long a step (relay bind, server start, tunnel up) may take.
const DEADLINE: Duration = Duration::from_secs(30);

/// One private home: config, state, and runtime dirs under a tempdir.
struct Home {
    dir: TempDir,
}

impl Home {
    fn new() -> Self {
        let dir = TempDir::new().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("run")).expect("runtime dir");
        Self { dir }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn env(&self) -> Vec<(&'static str, PathBuf)> {
        let dir = self.path();
        vec![
            ("HOME", dir.to_path_buf()),
            ("XDG_CONFIG_HOME", dir.join("config")),
            ("XDG_STATE_HOME", dir.join("state")),
            ("XDG_RUNTIME_DIR", dir.join("run")),
            ("PHUX_PROFILE", PathBuf::from("default")),
            ("PHUX_SOCKET", dir.join("s.sock")),
            ("PHUX_NO_AUTO_LISTEN", PathBuf::from("1")),
            // Any ssh or overlay probe fails loudly instead of reaching out.
            ("PHUX_SSH", dir.join("no-such-ssh")),
            ("PHUX_TAILSCALE", dir.join("no-such-tailscale")),
        ]
    }

    fn command(&self, args: &[&str]) -> std::process::Command {
        let mut cmd = common::phux_cmd(crate::runner::phux_bin());
        cmd.envs(self.env()).args(args).stdin(Stdio::null());
        cmd
    }

    fn phux(&self, args: &[&str]) -> Output {
        self.command(args).output().expect("run phux")
    }

    fn write_config(&self, body: &str) {
        let config = self.path().join("config/phux");
        std::fs::create_dir_all(&config).expect("config dir");
        std::fs::write(config.join("config.toml"), body).expect("write config");
    }

    fn config(&self) -> String {
        std::fs::read_to_string(self.path().join("config/phux/config.toml")).expect("config")
    }

    /// A mode-0600 file holding `secret`.
    fn secret_file(&self, name: &str, secret: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let path = self.path().join(name);
        std::fs::write(&path, format!("{secret}\n")).expect("write secret");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        path
    }
}

/// A child killed on drop, so a failed assertion leaves no daemon behind.
struct Reaped(Child);

impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The relay, the server behind it, and what a consumer needs to reach it.
struct Topology {
    _relay_home: Home,
    server_home: Home,
    _relay: Reaped,
    _server: Reaped,
    /// The relay's bound `HOST:PORT`.
    relay_addr: String,
    /// The relay certificate's fingerprint.
    relay_fp: String,
}

impl Topology {
    fn start() -> Self {
        let relay_home = Home::new();
        let (tunnel_token, relay_fp) = enroll_route(&relay_home);
        let (relay, relay_addr) = start_relay(&relay_home);

        let server_home = Home::new();
        let tunnel_file = server_home.secret_file("relay-route.token", &tunnel_token);
        server_home.write_config(&format!(
            "[[connector]]\nrelay = \"{relay_addr}\"\ntoken-file = \"{}\"\n\
             cert-fingerprint = \"{relay_fp}\"\n",
            tunnel_file.display()
        ));
        let server = server_home
            .command(&["server", "--exit-after-idle", "120"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn phux server");
        Self {
            _relay_home: relay_home,
            server_home,
            _relay: Reaped(relay),
            _server: Reaped(server),
            relay_addr,
            relay_fp,
        }
    }

    /// `phux pair --relay-route ROUTE --json` on the server host, retried
    /// until the freshly spawned server answers.
    fn pair(&self) -> serde_json::Value {
        let start = Instant::now();
        loop {
            let out = self.server_home.phux(&[
                "pair",
                "--relay-route",
                ROUTE,
                "--name",
                "mini",
                "--json",
            ]);
            if out.status.success() {
                return serde_json::from_slice(&out.stdout).expect("pair --json is JSON");
            }
            assert!(
                start.elapsed() < DEADLINE,
                "pair --relay-route never succeeded: {}",
                stderr(&out)
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// `phux relay pair --route ROUTE`: the tunnel token and the relay
/// certificate fingerprint, read from its human output.
fn enroll_route(home: &Home) -> (String, String) {
    let out = home.phux(&["relay", "pair", "--route", ROUTE]);
    assert!(out.status.success(), "relay pair: {}", stderr(&out));
    let text = stdout(&out);
    let after = |heading: &str| {
        let mut lines = text.lines();
        let found = lines
            .find(|line| line.starts_with(heading))
            .and_then(|_| lines.next());
        assert!(found.is_some(), "no {heading:?} section in: {text}");
        found.unwrap().trim().to_owned()
    };
    (after("Tunnel token"), after("Relay certificate SHA-256"))
}

/// `phux relay run` on an ephemeral loopback port; returns the child and the
/// address its banner reports. Stderr keeps draining on a thread so the
/// relay never blocks on a full pipe.
fn start_relay(home: &Home) -> (Child, String) {
    let mut relay = home
        .command(&["relay", "run", "--listen", "127.0.0.1:0"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn phux relay");
    let stderr = relay.stderr.take().expect("relay stderr");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if let Some(rest) = line.split("listening on ").nth(1) {
                let addr = rest.split_whitespace().next().unwrap_or_default();
                let _ = tx.send(addr.to_owned());
            }
        }
    });
    let addr = rx
        .recv_timeout(DEADLINE)
        .expect("the relay never printed its listening banner");
    (relay, addr)
}

/// Poll `phux ls --remote NAME --json` until it lists sessions: the tunnel
/// comes up asynchronously after the server starts.
fn list_over_relay(home: &Home, name: &str) -> serde_json::Value {
    let start = Instant::now();
    loop {
        let out = home.phux(&["ls", "--remote", name, "--json"]);
        if out.status.success() {
            return serde_json::from_slice(&out.stdout).expect("ls --json is JSON");
        }
        assert!(
            start.elapsed() < DEADLINE,
            "`ls --remote {name}` never reached the server through the relay: {}",
            stderr(&out)
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Run `phux attach --remote NAME --code LINK` on a PTY (attach sits behind
/// the TTY preflight) until it reports the pairing, then kill it: the attach
/// that follows would hold the PTY forever.
fn pair_from_code(home: &Home, name: &str, link: &str) -> String {
    let pty = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");
    let mut cmd = CommandBuilder::new(crate::runner::phux_bin());
    cmd.args(["attach", "--remote", name, "--code", link]);
    cmd.env_clear();
    if let Some(path) = std::env::var_os("PATH") {
        cmd.env("PATH", path);
    }
    if let Some(tmp) = std::env::var_os("TMPDIR") {
        cmd.env("TMPDIR", tmp);
    }
    for (key, value) in home.env() {
        cmd.env(key, value);
    }
    cmd.env("TERM", "xterm-256color");
    let mut child = pty.slave.spawn_command(cmd).expect("spawn attach");
    drop(pty.slave);
    let mut reader = pty.master.try_clone_reader().expect("clone reader");
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let needle = format!("paired {name} ->");
    let start = Instant::now();
    let mut seen = String::new();
    while !seen.contains(&needle) && start.elapsed() < DEADLINE {
        if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(100)) {
            seen.push_str(&String::from_utf8_lossy(&chunk));
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    assert!(
        seen.contains(&needle),
        "no pairing line from --code: {seen}"
    );
    seen
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `phux pair --relay-route` mints a link that dials the relay with the route
/// as SNI; `attach --remote --code` registers it as a routed entry; and the
/// session verbs reach the server through the real relay. The manual form,
/// `host add --tls-server-name`, reaches it too, and the same entry without
/// the route is refused at the relay: the route is what reaches the server.
#[test]
#[ignore = "spawns a real relay and server; runs in the e2e lane"]
fn a_relay_route_attaches_from_a_link_and_from_the_registry() {
    let topology = Topology::start();

    let doc = topology.pair();
    let link = doc["connect_link"].as_str().expect("a relay link");
    assert!(
        link.starts_with(&format!(
            "https://phux.sh/connect?quic=quic://{}&sni={ROUTE}&name=mini&fp=",
            topology.relay_addr
        )),
        "{link}"
    );
    assert!(
        !link.contains("url="),
        "a relay link has no WebSocket leg: {link}"
    );
    assert_eq!(doc["cert_fingerprint"], topology.relay_fp.as_str());
    assert_eq!(doc["relay"]["route"], ROUTE);
    assert_eq!(doc["relay"]["endpoint"], topology.relay_addr.as_str());
    let token = doc["token"].as_str().expect("token").to_owned();

    // From the link: the cold `--code` path registers the routed entry.
    let linked = Home::new();
    pair_from_code(&linked, "mini", link);
    let config = linked.config();
    assert!(
        config.contains(&format!("endpoint = \"quic://{}\"", topology.relay_addr))
            && config.contains(&format!("tls-server-name = \"{ROUTE}\"")),
        "{config}"
    );
    // ADR-0154: the link pins the server's CA beside the relay's leaf, the
    // client keeps it keyed by the route, and the listing above crossed the
    // relay inside an end-to-end session verified against it.
    let authority = doc["ca_fingerprint"].as_str().expect("the server's CA");
    assert!(link.contains(&format!("&ca={authority}&")), "{link}");
    let known = std::fs::read_to_string(linked.path().join("config/phux/known-authorities"))
        .expect("known-authorities");
    assert!(
        known.contains(&format!("@{ROUTE} {authority}")),
        "pinned by route: {known}"
    );
    let listed = list_over_relay(&linked, "mini");
    assert!(listed["sessions"].is_array(), "{listed}");

    // By hand: the same credentials through `host add --tls-server-name`.
    let manual = Home::new();
    let token_file = manual.secret_file("mini.token", &token);
    let endpoint = format!("quic://{}", topology.relay_addr);
    let token_path = token_file.display().to_string();
    let add = |name: &str, route: Option<&str>| {
        let mut args = vec![
            "host",
            "add",
            name,
            endpoint.as_str(),
            "--cert-fingerprint",
            topology.relay_fp.as_str(),
            "--token-file",
            token_path.as_str(),
        ];
        if let Some(route) = route {
            args.extend(["--tls-server-name", route]);
        }
        let out = manual.phux(&args);
        assert!(out.status.success(), "host add {name}: {}", stderr(&out));
    };
    add("routed", Some(ROUTE));
    add("unrouted", None);
    let show = manual.phux(&["host", "show", "routed", "--json"]);
    assert!(show.status.success(), "host show: {}", stderr(&show));
    let shown: serde_json::Value = serde_json::from_slice(&show.stdout).expect("show --json");
    assert_eq!(shown["host"]["tls_server_name"], ROUTE);
    let listed = list_over_relay(&manual, "routed");
    assert!(listed["sessions"].is_array(), "{listed}");

    let out = manual.phux(&["ls", "--remote", "unrouted", "--json"]);
    assert!(
        !out.status.success(),
        "without the route the relay must refuse the dial: {}",
        stdout(&out)
    );
    // The relay declines the unknown SNI at the TLS layer (ADR-0057).
    assert!(
        stderr(&out).contains("cryptographic handshake failed"),
        "{}",
        stderr(&out)
    );
}
