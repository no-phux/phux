//! Reverse enrollment through the CLI (ADR-0182): `phux host ticket` on a
//! `paired` hub mints a join code, `phux host join` on a server with no
//! listener enrolls with it and starts the uplink without a restart, and the
//! hub then spawns on that satellite. The code works once, and `phux
//! workload revoke t-…` withdraws an unused one. `host disable` drops the
//! link and `host enable` lets the satellite back in; `host rename` is
//! refused; `host rm` revokes the credential.
//!
//! Two hermetic machines in two temp dirs, each with its own config, state,
//! and socket.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

use crate::{common, listeners};

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tempfile::TempDir;

use crate::common::Out;
use crate::hermetic::{self, Server, phux_with, prepare_dirs};

/// How long the uplink has to come up after `join` or `host enable`; the
/// satellite's redial backoff caps at 30 s.
const LINK_DEADLINE: Duration = Duration::from_secs(60);

fn socket(dir: &Path) -> PathBuf {
    dir.join("s.sock")
}

/// `phux ARGS` in `dir` with `input` on stdin, dialing `dir`'s socket.
fn phux_with_stdin(dir: &Path, args: &[&str], input: &str) -> Out {
    phux_with(dir, args, input.as_bytes(), &[("PHUX_SOCKET", socket(dir))]).into()
}

fn phux(dir: &Path, args: &[&str]) -> Out {
    phux_with_stdin(dir, args, "")
}

fn start_server(dir: &Path, extra: &[&str]) -> Server {
    let socket = socket(dir);
    hermetic::start_server(
        dir,
        extra,
        &[("PHUX_SOCKET", socket.to_str().expect("UTF-8 socket path"))],
    )
}

/// Wait until the server at `dir` answers.
fn await_server(dir: &Path) {
    let answered = common::until(LINK_DEADLINE, || {
        phux(dir, &["ls", "--json"]).success().then_some(())
    });
    assert!(answered.is_some(), "the server never answered");
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
    assert!(init.success(), "{}", init.stderr);
    let server = start_server(dir, &["--quic", listeners::LOOPBACK_ANY_PORT]);
    let quic =
        listeners::bound_listener_addr(&socket(dir), listeners::RemoteListenerTransport::Quic);
    (server, quic.to_string())
}

/// `phux host ticket sat --hub QUIC --json` on the hub.
fn ticket(hub: &Path, quic: &str) -> serde_json::Value {
    phux(hub, &["host", "ticket", "sat", "--hub", quic, "--json"]).json()
}

/// Retry spawning on satellite `sat` until it fails (a reload is applied
/// asynchronously); the failing output.
fn spawn_refused(hub: &Path) -> Out {
    let start = Instant::now();
    loop {
        let out = phux(
            hub,
            &["spawn", "--satellite", "sat", "--json", "--", "/bin/cat"],
        );
        if !out.success() {
            return out;
        }
        assert!(
            start.elapsed() < LINK_DEADLINE,
            "the hub still reaches the satellite"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Whether `host ls` on `dir` lists a host named `name`.
fn host_listed(dir: &Path, name: &str) -> bool {
    let listed = phux(dir, &["host", "ls", "--json"]).json();
    listed["hosts"]
        .as_array()
        .expect("hosts")
        .iter()
        .any(|host| host["name"] == name)
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
        if out.success() {
            return out.json();
        }
        assert!(
            start.elapsed() < LINK_DEADLINE,
            "the hub never reached the satellite: {}",
            out.stderr
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Whether `workload list` on `hub` shows `credential` with `key` = `value`.
fn credential_has(hub: &Path, credential: &str, key: &str, value: &str) -> bool {
    let workloads = phux(hub, &["workload", "list", "--json"]).json();
    workloads["credentials"]
        .as_array()
        .expect("credentials")
        .iter()
        .any(|row| row["credential_id"] == credential && row[key] == value)
}

/// Bad input to `host ticket` writes nothing: a hub endpoint with a scheme,
/// and a name a dialed satellite already holds.
fn bad_tickets_write_nothing(hub: &Path, quic: &str) {
    let bad_hub = phux(
        hub,
        &[
            "host",
            "ticket",
            "other",
            "--hub",
            "quic://127.0.0.1:9",
            "--json",
        ],
    );
    assert!(!bad_hub.success(), "a scheme in --hub is refused");
    assert!(!host_listed(hub, "other"), "nothing was registered");
    let dialed = phux(
        hub,
        &[
            "host",
            "add",
            "--role",
            "satellite",
            "lab",
            "quic://127.0.0.1:9",
            "--json",
        ],
    );
    assert!(dialed.success(), "{}", dialed.stderr);
    let clobber = phux(hub, &["host", "ticket", "lab", "--hub", quic, "--json"]);
    assert!(!clobber.success(), "a dialed satellite is not replaced");
    let hub_config =
        std::fs::read_to_string(hub.join("config/phux/config.toml")).expect("hub config");
    assert!(hub_config.contains("quic://127.0.0.1:9"), "{hub_config}");
}

/// Rename, disable, enable, a satellite-side restart, and rm on the joined
/// satellite `sat`.
fn manage_the_joined_satellite(hub: &Path, sat: &Path, sat_server: Server, credential: &str) {
    // The credential is bound to the name, so rename is refused.
    let renamed = phux(hub, &["host", "rename", "sat", "sat2", "--json"]);
    assert!(!renamed.success(), "an inbound satellite is not renamed");
    assert!(host_listed(hub, "sat"));

    // Disable drops the live link and refuses re-adoption; enable lets the
    // redialing satellite back in.
    phux(hub, &["host", "disable", "sat", "--json"]).json();
    let refused = spawn_refused(hub);
    assert!(
        refused.stdout.contains("satellite") || refused.stderr.contains("satellite"),
        "{}{}",
        refused.stdout,
        refused.stderr
    );
    std::thread::sleep(Duration::from_secs(2));
    assert!(
        !phux(
            hub,
            &["spawn", "--satellite", "sat", "--json", "--", "/bin/cat"]
        )
        .success(),
        "a disabled satellite is not re-adopted when it redials"
    );
    phux(hub, &["host", "enable", "sat", "--json"]).json();
    spawn_on_satellite(hub);

    // A satellite-side drop: the restarted server reads `[[uplink]]` and
    // dials the hub again, superseding the link the killed one left behind.
    drop(sat_server);
    let _sat_server = start_server(sat, &[]);
    await_server(sat);
    spawn_on_satellite(hub);

    // Remove revokes the credential and ends the link.
    let removed = phux(hub, &["host", "rm", "sat", "--json"]).json();
    assert_eq!(removed["revoked_credentials"], 1, "{removed}");
    spawn_refused(hub);
    assert!(
        credential_has(hub, credential, "status", "revoked"),
        "the removed satellite's credential is revoked"
    );
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
    let revoked = phux(hub.path(), &["workload", "revoke", &ticket_id, "--json"]).json();
    assert_eq!(revoked["newly_revoked"], true, "{revoked}");

    let sat_server = start_server(sat.path(), &[]);
    await_server(sat.path());
    let stale = withdrawn["join_code"].as_str().expect("join code");
    let refused = phux(sat.path(), &["host", "join", stale, "--json"]);
    assert!(!refused.success(), "a revoked code enrolled");

    let minted = ticket(hub.path(), &quic);
    assert_eq!(minted["satellite"], "sat");
    let code = minted["join_code"].as_str().expect("join code").to_owned();
    let ticket_secret = code.rsplit("enroll=").next().expect("ticket").to_owned();
    let listed = phux(hub.path(), &["host", "ls", "--json"]).json();
    assert!(
        listed["hosts"]
            .as_array()
            .expect("hosts")
            .iter()
            .any(|host| host["name"] == "sat" && host["endpoint"] == "inbound://"),
        "{listed}"
    );

    bad_tickets_write_nothing(hub.path(), &quic);

    // The code goes in on stdin, out of `ps` and shell history.
    let joined_out = phux_with_stdin(sat.path(), &["host", "join", "-", "--json"], &code);
    assert!(
        !joined_out.stdout.contains(&ticket_secret) && !joined_out.stderr.contains(&ticket_secret),
        "the ticket is never echoed"
    );
    let joined = joined_out.json();
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
    assert!(!again.success(), "a code enrolls once");

    // The satellite has no listener; the hub reaches it over the uplink.
    let spawned = spawn_on_satellite(hub.path());
    assert_eq!(spawned["satellite"], "sat", "{spawned}");

    let credential = joined["credential_id"].as_str().expect("credential");
    assert!(credential_has(hub.path(), credential, "satellite", "sat"));

    manage_the_joined_satellite(hub.path(), sat.path(), sat_server, credential);
}
