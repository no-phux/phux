//! Desktop remote attach against a real, enrolled phux server (phux-c2td.17).
//!
//! The "remote host" is a checkout-built `phux server` with its own home,
//! config, state, and runtime directories, listening on loopback QUIC and
//! secure WebSocket. `phux pair --json` mints its credential and pin;
//! `phux host add NAME quic://… --token-file … --cert-fingerprint …` writes
//! the "local machine's" registry in a second, disjoint set of directories.
//! The window then takes the desktop's own path: [`super::home`] picks the
//! home session, [`Connection::connect_remote_with_key`] attaches it, and the
//! journey splits, types, renames, reconnects, and relaunches with the
//! persistent key over each lane. Then the host is killed and restarted on
//! the same ports and state: the window redials it (and, pinned to a home
//! that is no longer live, refuses to rebind; phux-c2td.17.1), and a fresh
//! window reopens the same home. Last, a stale pin and a revoked token are
//! refused with their own reasons.
//!
//! What this cannot show is a second machine: a routed network with NAT,
//! loss, and real latency, and a host enrolled over ssh. That run is still
//! manual (see `clients/desktop/README.md`).
//!
//! The registry is read from `$XDG_CONFIG_HOME`, and this crate denies
//! `unsafe` (so `set_var`), so the outer test re-runs itself with the client
//! directories in its environment. Ignored by default, like the window
//! journeys: it needs `PHUX_DESKTOP_JOURNEY_TEST_BIN`.

use super::*;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const CHILD: &str = "PHUX_DESKTOP_REMOTE_ACCEPTANCE_ROOT";
const PHUX_BIN: &str = "PHUX_DESKTOP_JOURNEY_TEST_BIN";
const TEST: &str =
    "connection::remote_acceptance::desktop_attaches_an_enrolled_host_over_quic_and_wss";
const DEADLINE: Duration = Duration::from_secs(30);

/// Every phux process here starts from this: no inherited `PHUX_*` (a phux
/// pane exports the operator's socket and credential store), and `TMPDIR`
/// set to `/tmp` so the short sandbox under it is not production state to a
/// development build, and the socket path fits `SUN_LEN`.
fn scrubbed(program: &Path, home: &Path) -> Command {
    let mut command = Command::new(program);
    for (key, _) in std::env::vars_os() {
        if key.as_encoded_bytes().starts_with(b"PHUX_") {
            command.env_remove(key);
        }
    }
    command
        .env("TMPDIR", "/tmp")
        .env("HOME", home.join("home"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_STATE_HOME", home.join("state"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_RUNTIME_DIR", home.join("runtime"))
        .env("PHUX_TAILSCALE", "phux-test-no-such-overlay-command")
        .env("PHUX_NO_UPDATE_CHECK", "1")
        .stdin(Stdio::null());
    command
}

fn sandbox(root: &Path) {
    for name in ["home", "config", "state", "data", "runtime"] {
        std::fs::create_dir_all(root.join(name)).unwrap();
    }
}

#[test]
#[ignore = "requires PHUX_DESKTOP_JOURNEY_TEST_BIN pointing at a checkout-built CLI"]
fn desktop_attaches_an_enrolled_host_over_quic_and_wss() {
    if let Some(root) = std::env::var_os(CHILD) {
        journey(Path::new(&root));
        return;
    }
    let phux = std::env::var_os(PHUX_BIN).expect("a checkout-built phux CLI to serve");
    let root = PathBuf::from(format!("/tmp/pdra-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let client = root.join("client");
    sandbox(&client);
    let output = scrubbed(&std::env::current_exe().unwrap(), &client)
        .env(CHILD, &root)
        .env(PHUX_BIN, &phux)
        .args([TEST, "--exact", "--ignored", "--nocapture"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    eprintln!("{stdout}{stderr}");
    let _ = std::fs::remove_dir_all(&root);
    assert!(output.status.success(), "the journey failed");
    assert!(stdout.contains("1 passed"), "the journey did not run");
}

/// The remote host: one `phux server` process and its private directories.
struct Host {
    phux: PathBuf,
    dir: PathBuf,
    child: Option<Child>,
}

impl Host {
    fn socket(&self) -> PathBuf {
        self.dir.join("s.sock")
    }

    fn command(&self) -> Command {
        let mut command = scrubbed(&self.phux, &self.dir);
        command.env("PHUX_SOCKET", self.socket());
        command
    }

    /// Serve on `quic` and `wss` (`127.0.0.1:0` picks a port).
    fn start(&mut self, quic: &str, wss: &str) {
        let log = std::fs::File::create(self.dir.join("server.log")).unwrap();
        let child = self
            .command()
            .env("SHELL", "/bin/sh")
            .env("PHUX_WS_SECURE", "1")
            .env("PHUX_NO_AUTO_LISTEN", "1")
            .args(["server", "--quic", quic, "--listen", wss])
            .args(["--exit-after-idle", "120"])
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap();
        self.child = Some(child);
    }

    /// The link drops the way a crashed or rebooted host drops it.
    fn kill(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::fs::remove_file(self.socket());
    }

    fn phux(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    /// `phux pair --json`, once the listeners have bound.
    fn pair(&self) -> serde_json::Value {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let output = self.phux(&["pair", "--json"]);
            if output.status.success() {
                return serde_json::from_slice(&output.stdout).unwrap();
            }
            assert!(
                Instant::now() < deadline,
                "phux pair never minted: {}\n{}",
                String::from_utf8_lossy(&output.stderr),
                std::fs::read_to_string(self.dir.join("server.log")).unwrap_or_default()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.kill();
    }
}

/// `phux host add NAME ENDPOINT` on the client side (this process's env).
fn register(phux: &Path, name: &str, endpoint: &str, token: &Path, fingerprint: &str) {
    let output = Command::new(phux)
        .args(["host", "add", name, endpoint, "--token-file"])
        .arg(token)
        .args(["--cert-fingerprint", fingerprint])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "phux host add {name}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn field<'a>(document: &'a serde_json::Value, key: &str) -> &'a str {
    document[key]
        .as_str()
        .unwrap_or_else(|| panic!("pair --json has no {key}: {document}"))
}

/// Drain until `done`, waking on the runtime's activity listener.
fn wait(connection: &mut Connection, what: &str, mut done: impl FnMut(&Connection) -> bool) {
    let deadline = Instant::now() + DEADLINE;
    let wake = connection.activity();
    loop {
        connection.drain();
        if done(connection) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: status={:?} error={:?} session={:?} home={:?} topology={:#?}",
            connection.status,
            connection.error,
            connection.session_name(),
            connection.home_session(),
            connection.topology
        );
        smol::block_on(smol::future::or(
            async {
                let _ = wake.recv().await;
            },
            async {
                smol::Timer::after(Duration::from_millis(100)).await;
            },
        ));
    }
}

fn attached_home(connection: &Connection) -> bool {
    connection.status == Status::Attached
        && connection.home_session().is_some()
        && connection.session_validated()
}

fn home_panes(connection: &Connection) -> Vec<ResourceId> {
    let home = connection.home_session();
    connection
        .panes()
        .iter()
        .filter(|pane| Some(pane.session_id) == home)
        .map(|pane| pane.terminal_id.clone())
        .collect()
}

fn home_key_of(connection: &Connection) -> SessionKey {
    let topology = connection.topology.as_ref().unwrap();
    let id = connection.home_session().unwrap();
    topology
        .sessions
        .iter()
        .find(|session| session.id == id)
        .and_then(|session| session.key)
        .expect("an ORGANIZATION server keys its sessions")
}

/// Type a line into `terminal` and read it back from the desktop's view.
fn round_trip(connection: &mut Connection, terminal: &ResourceId, marker: &str) {
    let view = connection.client.create_view(terminal).unwrap();
    // The shell computes the marker, so the echo of the typed line is not it.
    let (left, right) = marker.split_at(marker.len() / 2);
    let line = format!("printf '%s%s\\n' {left} {right}");
    assert_ne!(connection.client.apply_line(terminal, &line), 0);
    wait(
        connection,
        &format!("{marker} on {terminal:?}"),
        |connection| {
            connection
                .client
                .acquire_view(view)
                .is_some_and(|frame| frame.text().contains(marker))
        },
    );
    let _ = connection.client.destroy_view(view);
}

/// Spawn a terminal into the home session, the way a split does, and wait
/// until the window can type into it.
fn split(window: &mut Connection) -> ResourceId {
    let before = home_panes(window);
    window
        .spawn(SpawnRequest {
            command: None,
            cwd: None,
            env: None,
            session_id: window.home_session(),
            initial_size: Some((80, 24)),
        })
        .unwrap();
    let mut split = None;
    wait(window, "the split pane", |window| {
        split = home_panes(window)
            .into_iter()
            .find(|pane| !before.contains(pane) && window.ready(pane));
        split.is_some()
    });
    split.unwrap()
}

/// Attach `host`, split, type, rename, reconnect, and relaunch by key.
/// Returns the relaunched window, attached.
fn attach_journey(host: &str, renamed: &str) -> Connection {
    let name = home::open_remote_home(host).unwrap();
    let mut window = Connection::connect_remote_with_key(host, name, None).unwrap();
    wait(&mut window, "the home session over the link", attached_home);
    let server = window.server_id().unwrap();

    // Split: a second terminal in the home session, live and typeable.
    let split = split(&mut window);
    round_trip(&mut window, &split, &format!("{host}-split-ok"));

    // Rename through ORGANIZATION; the record keeps its key.
    let key = home_key_of(&window);
    window.client.rename_session(key, renamed).unwrap();
    wait(&mut window, "the rename", |window| {
        window
            .topology
            .as_ref()
            .and_then(|topology| topology.session_keyed(&key))
            .is_some_and(|session| session.name == renamed)
    });

    // An explicit reconnect is a new epoch on the same server incarnation.
    let epoch = window.connection_epoch();
    window.reconnect().unwrap();
    wait(&mut window, "the reconnect", |window| {
        window.connection_epoch() > epoch
            && window.status == Status::Attached
            && window.ready(&split)
    });
    assert_eq!(window.server_id(), Some(server.clone()));
    round_trip(&mut window, &split, &format!("{host}-reconnect-ok"));
    window.close();

    // Relaunch: the saved key finds the renamed home, as `Desk` restores it.
    let name = home::existing_remote_home(host, "stale-name", Some(key)).unwrap();
    assert_eq!(name, renamed);
    let mut window = Connection::connect_remote_with_key(host, name, Some(key)).unwrap();
    wait(&mut window, "the relaunched window", |window| {
        attached_home(window) && window.ready(&split)
    });
    assert_eq!(window.home_key(), Some(key));
    assert_eq!(window.server_id(), Some(server));
    round_trip(&mut window, &split, &format!("{host}-relaunch-ok"));
    window
}

fn journey(root: &Path) {
    let phux = PathBuf::from(std::env::var_os(PHUX_BIN).unwrap());
    let dir = root.join("remote");
    sandbox(&dir);
    let mut host = Host {
        phux: phux.clone(),
        dir,
        child: None,
    };
    host.start("127.0.0.1:0", "127.0.0.1:0");
    let pair = host.pair();
    let fingerprint = field(&pair, "cert_fingerprint").to_owned();
    let quic = field(&pair, "quic_addr").to_owned();
    let wss = field(&pair, "ws_addr").to_owned();
    let token = root.join("client").join("mini.token");
    std::fs::write(&token, format!("{}\n", field(&pair, "token"))).unwrap();
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    register(
        &phux,
        "mini",
        &format!("quic://{quic}"),
        &token,
        &fingerprint,
    );
    register(
        &phux,
        "mini-wss",
        &format!("wss://{wss}"),
        &token,
        &fingerprint,
    );
    let wrong = ["00"; 32].join(":");
    assert_ne!(wrong, fingerprint);
    register(&phux, "stale", &format!("quic://{quic}"), &token, &wrong);

    attach_journey("mini-wss", "renamed-over-wss").close();
    let mut window = attach_journey("mini", "renamed-over-quic");
    let key = home_key_of(&window);
    let server = window.server_id().unwrap();

    // Link drop: the host dies and comes back on the same ports and state.
    host.kill();
    wait(&mut window, "the link drop", |window| {
        window.status != Status::Attached
    });
    host.start(&quic, &wss);
    // The window redials the new incarnation on its own. Its pinned home is
    // a persistent record with no live session after a restart, and a
    // pinned window refuses to rebind by name (phux-c2td.17.1).
    wait(&mut window, "the redial after the host restart", |window| {
        window.status.is_terminal()
    });
    assert_ne!(window.server_id(), Some(server), "never redialed");
    let refusal = window.error.clone().unwrap_or_default();
    assert!(refusal.contains("refusing a name replacement"), "{refusal}");

    // A fresh window reopens the same persistent home on the restarted host.
    let name = home::open_remote_home("mini").unwrap();
    assert_eq!(name, "renamed-over-quic");
    let mut window = Connection::connect_remote_with_key("mini", name, None).unwrap();
    wait(
        &mut window,
        "a fresh window after the host restart",
        attached_home,
    );
    assert_eq!(home_key_of(&window), key, "the same persistent home");
    let terminal = split(&mut window);
    round_trip(&mut window, &terminal, "after-restart-ok");
    window.close();

    // A pin that does not match the host's certificate never attaches.
    let refused = home::open_remote_home("stale").unwrap_err();
    assert!(refused.contains("fingerprint mismatch"), "{refused}");

    // A revoked credential is refused on the next dial, over both lanes.
    let revoked = host.phux(&["pair", "revoke", field(&pair, "credential_id"), "--json"]);
    assert!(
        revoked.status.success(),
        "{}",
        String::from_utf8_lossy(&revoked.stderr)
    );
    let refused = home::open_remote_home("mini").unwrap_err();
    assert!(refused.contains("refused the pairing token"), "{refused}");
    let refused = home::open_remote_home("mini-wss").unwrap_err();
    assert!(refused.contains("401"), "{refused}");
}
