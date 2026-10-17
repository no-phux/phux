//! Reverse enrollment through the CLI (ADR-0180): `phux host ticket` on a
//! `paired` hub mints a join code, `phux host join` on a server with no
//! listener enrolls with it and starts the uplink without a restart, and the
//! hub then spawns on that satellite. The code works once, and `phux
//! workload revoke t-…` withdraws an unused one.
//!
//! Two hermetic machines in two temp dirs, each with its own config, state,
//! and socket.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

#[path = "../common/ambient.rs"]
mod common;
#[path = "../common/listeners.rs"]
mod listeners;

use std::path::{Path, PathBuf};
use std::process::{Child, Output, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

use crate::hermetic::{hermetic_env, prepare_dirs};

/// How long the uplink has to come up after `join`.
const LINK_DEADLINE: Duration = Duration::from_secs(30);

/// A running `phux server`, killed on drop.
struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn socket(dir: &Path) -> PathBuf {
    dir.join("s.sock")
}

fn env(dir: &Path) -> Vec<(&'static str, PathBuf)> {
    let mut env = hermetic_env(dir);
    env.push(("PHUX_SOCKET", socket(dir)));
    env
}

fn phux(dir: &Path, args: &[&str]) -> Output {
    common::phux_cmd(crate::runner::phux_bin())
        .envs(env(dir))
        .current_dir(dir)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run phux")
}

fn start_server(dir: &Path, extra: &[&str]) -> Server {
    let child = common::phux_cmd(crate::runner::phux_bin())
        .envs(env(dir))
        .current_dir(dir)
        .arg("server")
        .args(extra)
        .args(["--exit-after-idle", "120"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn phux server");
    Server(child)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn json_doc(out: &Output) -> serde_json::Value {
    assert!(
        out.status.success(),
        "{}{}",
        text(&out.stdout),
        text(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("one JSON document")
}

/// Wait until the server at `dir` answers.
fn await_server(dir: &Path) {
    let start = Instant::now();
    while !phux(dir, &["ls", "--json"]).status.success() {
        assert!(start.elapsed() < LINK_DEADLINE, "the server never answered");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Start a `paired` hub with a CA and a loopback QUIC listener in `dir`; the
/// server and the address the listener bound.
fn start_paired_hub(dir: &Path) -> (Server, String) {
    std::fs::write(
        dir.join("config/phux/config.toml"),
        "[policy]\nmode = \"paired\"\n",
    )
    .expect("hub config");
    let init = phux(dir, &["workload", "authority", "--init"]);
    assert!(init.status.success(), "{}", text(&init.stderr));
    let server = start_server(dir, &["--quic", listeners::LOOPBACK_ANY_PORT]);
    let quic =
        listeners::bound_listener_addr(&socket(dir), listeners::RemoteListenerTransport::Quic);
    (server, quic.to_string())
}

/// `phux host ticket sat --hub QUIC --json` on the hub.
fn ticket(hub: &Path, quic: &str) -> serde_json::Value {
    json_doc(&phux(
        hub,
        &["host", "ticket", "sat", "--hub", quic, "--json"],
    ))
}

/// Spawn `/bin/cat` on satellite `sat` through the hub, retrying while the
/// uplink comes up.
fn spawn_on_satellite(hub: &Path) -> serde_json::Value {
    let start = Instant::now();
    loop {
        let out = phux(
            hub,
            &["spawn", "--satellite", "sat", "--json", "--", "/bin/cat"],
        );
        if out.status.success() {
            return serde_json::from_slice(&out.stdout).expect("json");
        }
        assert!(
            start.elapsed() < LINK_DEADLINE,
            "the hub never reached the satellite: {}",
            text(&out.stderr)
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
#[ignore = "spawns two real servers, one a paired QUIC hub; runs in the e2e lane"]
fn a_satellite_joins_with_a_code_and_the_hub_spawns_on_it() {
    let hub = TempDir::new().expect("hub dir");
    let sat = TempDir::new().expect("satellite dir");
    for dir in [hub.path(), sat.path()] {
        prepare_dirs(dir);
    }
    let (_hub_server, quic) = start_paired_hub(hub.path());

    // An unused code is withdrawn, then refused.
    let withdrawn = ticket(hub.path(), &quic);
    let ticket_id = withdrawn["ticket_id"]
        .as_str()
        .expect("ticket id")
        .to_owned();
    let revoked = json_doc(&phux(
        hub.path(),
        &["workload", "revoke", &ticket_id, "--json"],
    ));
    assert_eq!(revoked["newly_revoked"], true, "{revoked}");

    let _sat_server = start_server(sat.path(), &[]);
    await_server(sat.path());
    let stale = withdrawn["join_code"].as_str().expect("join code");
    let refused = phux(sat.path(), &["host", "join", stale, "--json"]);
    assert!(!refused.status.success(), "a revoked code enrolled");

    let minted = ticket(hub.path(), &quic);
    assert_eq!(minted["satellite"], "sat");
    let code = minted["join_code"].as_str().expect("join code").to_owned();
    let ticket_secret = code.rsplit("enroll=").next().expect("ticket").to_owned();
    let listed = json_doc(&phux(hub.path(), &["host", "ls", "--json"]));
    assert!(
        listed["hosts"]
            .as_array()
            .expect("hosts")
            .iter()
            .any(|host| host["name"] == "sat" && host["endpoint"] == "inbound://"),
        "{listed}"
    );

    let joined_out = phux(sat.path(), &["host", "join", &code, "--json"]);
    assert!(
        !text(&joined_out.stdout).contains(&ticket_secret)
            && !text(&joined_out.stderr).contains(&ticket_secret),
        "the ticket is never echoed"
    );
    let joined = json_doc(&joined_out);
    assert_eq!(joined["server_reload"], "signalled", "{joined}");
    let config = std::fs::read_to_string(sat.path().join("config/phux/config.toml"))
        .expect("satellite config");
    assert!(config.contains("[[uplink]]"), "{config}");
    let key = PathBuf::from(joined["client_key"].as_str().expect("key path"));
    let mode = std::os::unix::fs::PermissionsExt::mode(
        &std::fs::metadata(&key).expect("key file").permissions(),
    );
    assert_eq!(mode & 0o777, 0o600);

    let again = phux(sat.path(), &["host", "join", &code, "--json"]);
    assert!(!again.status.success(), "a code enrolls once");

    // The satellite has no listener; the hub reaches it over the uplink.
    let spawned = spawn_on_satellite(hub.path());
    assert_eq!(spawned["satellite"], "sat", "{spawned}");

    let workloads = json_doc(&phux(hub.path(), &["workload", "list", "--json"]));
    let credential = joined["credential_id"].as_str().expect("credential");
    assert!(
        workloads["credentials"]
            .as_array()
            .expect("credentials")
            .iter()
            .any(|row| row["credential_id"] == credential && row["satellite"] == "sat"),
        "{workloads}"
    );
}
