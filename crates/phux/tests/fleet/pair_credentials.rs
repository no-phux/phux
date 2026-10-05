//! Real-CLI credential lifecycle and custom-store integrity coverage, plus
//! the live-listener gate every mint passes first (ADR-0141).

#![allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Output, Stdio};

/// The socket every command in one test dials: beside the test's state
/// dir, never the operator's.
fn socket_path(state: &Path) -> PathBuf {
    state.with_file_name("s.sock")
}

/// A real `phux server` on the test's socket with a secure loopback wss
/// listener: the bound remote listener a mint requires. Killed on drop.
struct Server {
    child: Child,
    socket: PathBuf,
    /// The wss listener's `127.0.0.1:PORT`.
    wss_addr: String,
}

impl Server {
    fn start(state: &Path, tokens: Option<&Path>) -> Self {
        let socket = socket_path(state);
        let mut command = crate::common::phux_cmd(crate::runner::phux_bin());
        command
            .env("XDG_STATE_HOME", state)
            .env("PHUX_TAILSCALE", "phux-test-no-such-overlay-command")
            .env("PHUX_NO_AUTO_LISTEN", "1")
            // The routable path (TLS + bearer token) on loopback.
            .env("PHUX_WS_SECURE", "1")
            .arg("server")
            .arg("--socket")
            .arg(&socket)
            .args(["--no-seed", "--listen", crate::listeners::LOOPBACK_ANY_PORT])
            // Backstop only: Drop kills the server.
            .args(["--exit-after-idle", "120"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        match tokens {
            Some(tokens) => {
                command.env("PHUX_WS_TOKENS", tokens);
            }
            None => {
                command.env_remove("PHUX_WS_TOKENS");
            }
        }
        let child = command.spawn().expect("spawn phux server");
        let mut server = Self {
            child,
            socket,
            wss_addr: String::new(),
        };
        server.wss_addr = crate::listeners::bound_listener_addr(
            &server.socket,
            crate::listeners::RemoteListenerTransport::Wss,
        )
        .to_string();
        server
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// Run the real `phux` binary against an isolated state dir.
///
/// `tokens: None` means "use the DEFAULT store under `state`", and saying so
/// requires actively removing `PHUX_WS_TOKENS` from the inherited
/// environment — a child process inherits the parent's env, and this suite's
/// most likely reader is a maintainer running it from inside a phux pane,
/// where the service manager exports `PHUX_WS_TOKENS` pointing at their REAL
/// credential store. Without the removal the "default store" cases silently
/// operate on that store instead: the failure observed was `phux pair
/// --json` refusing with "legacy token store requires explicit migration",
/// and the case that mints successfully would go on to chmod 0o640 a live
/// credential file. The env is scrubbed rather than cleared wholesale
/// because `PATH` and friends still have to reach the child. `PHUX_SOCKET`
/// is pinned to the test's own socket for the same reason: a mint dials it.
fn phux(state: &Path, tokens: Option<&Path>, args: &[&str]) -> Output {
    let mut command = crate::common::phux_cmd(crate::runner::phux_bin());
    command
        .env("XDG_STATE_HOME", state)
        .env("PHUX_SOCKET", socket_path(state))
        .env("PHUX_TAILSCALE", "phux-test-no-such-overlay-command")
        .args(args);
    match tokens {
        Some(tokens) => {
            command.env("PHUX_WS_TOKENS", tokens);
        }
        None => {
            command.env_remove("PHUX_WS_TOKENS");
        }
    }
    command.output().expect("run phux pair")
}

fn json(output: &Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "stderr={} stdout={}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    serde_json::from_slice(&output.stdout).expect("stdout is one JSON document")
}

fn bearer(encoded: &str) -> Vec<u8> {
    let (pairs, _) = encoded.as_bytes().as_chunks::<2>();
    pairs
        .iter()
        .map(|pair| {
            let text = std::str::from_utf8(pair).unwrap();
            u8::from_str_radix(text, 16).unwrap()
        })
        .collect()
}

#[test]
fn custom_store_mint_rotate_revoke_is_operational_and_secret_safe() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let tokens = dir.path().join("custom-credentials");
    let _server = Server::start(&state, Some(&tokens));

    let minted_output = phux(&state, Some(&tokens), &["pair", "--json"]);
    let minted = json(&minted_output);
    let id = minted["credential_id"].as_str().unwrap();
    let old_token = minted["token"].as_str().unwrap();
    assert_eq!(minted["generation"], 1);
    assert_eq!(minted["tokens_path"], tokens.display().to_string());

    let rotated_output = phux(
        &state,
        Some(&tokens),
        &["pair", "rotate", id, "--overlap-seconds", "60", "--json"],
    );
    let rotated = json(&rotated_output);
    let new_token = rotated["token"].as_str().unwrap();
    assert_eq!(rotated["operation"], "rotate");
    assert_eq!(rotated["credential_id"], id);
    assert_eq!(rotated["generation"], 2);
    assert_eq!(rotated["overlap_seconds"], 60);
    assert_ne!(new_token, old_token);
    assert!(!String::from_utf8_lossy(&rotated_output.stdout).contains(old_token));
    assert!(!String::from_utf8_lossy(&rotated_output.stderr).contains(old_token));

    let store = phux_server::auth::TokenStore::load(&tokens).unwrap();
    assert!(store.verify(&bearer(old_token)));
    assert!(store.verify(&bearer(new_token)));

    let revoked_output = phux(&state, Some(&tokens), &["pair", "revoke", id, "--json"]);
    let revoked = json(&revoked_output);
    assert_eq!(revoked["operation"], "revoke");
    assert_eq!(revoked["credential_id"], id);
    let revoke_streams = format!(
        "{}{}",
        String::from_utf8_lossy(&revoked_output.stdout),
        String::from_utf8_lossy(&revoked_output.stderr)
    );
    assert!(!revoke_streams.contains(old_token));
    assert!(!revoke_streams.contains(new_token));

    let store = phux_server::auth::TokenStore::load(&tokens).unwrap();
    assert!(!store.verify(&bearer(old_token)));
    assert!(!store.verify(&bearer(new_token)));
}

#[test]
fn expired_rotation_emits_no_secret_and_leaves_the_store_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let tokens = dir.path().join("custom-credentials");
    let _server = Server::start(&state, Some(&tokens));
    let minted = json(&phux(&state, Some(&tokens), &["pair", "--json"]));
    let id = minted["credential_id"].as_str().unwrap().to_owned();

    let mut store: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&tokens).unwrap()).unwrap();
    store["credentials"][0]["expires_at"] = serde_json::json!("2000-01-01T00:00:00Z");
    std::fs::write(&tokens, serde_json::to_vec_pretty(&store).unwrap()).unwrap();
    let before = std::fs::read(&tokens).unwrap();

    let denied = phux(&state, Some(&tokens), &["pair", "rotate", &id, "--json"]);
    assert!(!denied.status.success());
    assert!(
        denied.stdout.is_empty(),
        "failed JSON action emits no document"
    );
    assert!(String::from_utf8_lossy(&denied.stderr).contains("expired"));
    assert!(!String::from_utf8_lossy(&denied.stderr).contains("token"));
    assert_eq!(std::fs::read(&tokens).unwrap(), before);
}

#[test]
fn default_and_environment_selected_stores_refuse_unsafe_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let _server = Server::start(&state, None);

    let default_minted = json(&phux(&state, None, &["pair", "--json"]));
    let default_path = std::path::PathBuf::from(default_minted["tokens_path"].as_str().unwrap());
    let default_id = default_minted["credential_id"].as_str().unwrap();
    std::fs::set_permissions(&default_path, std::fs::Permissions::from_mode(0o640)).unwrap();
    let denied = phux(&state, None, &["pair", "revoke", default_id]);
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("insecure credential store"));
    assert!(
        !String::from_utf8_lossy(&denied.stderr)
            .contains(default_minted["token"].as_str().unwrap())
    );

    let custom_path = dir.path().join("custom-credentials");
    let custom_minted = json(&phux(&state, Some(&custom_path), &["pair", "--json"]));
    let custom_id = custom_minted["credential_id"].as_str().unwrap();
    std::fs::set_permissions(&custom_path, std::fs::Permissions::from_mode(0o604)).unwrap();
    let denied = phux(&state, Some(&custom_path), &["pair", "rotate", custom_id]);
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("insecure credential store"));
    assert!(
        !String::from_utf8_lossy(&denied.stderr).contains(custom_minted["token"].as_str().unwrap())
    );
}

#[test]
fn pair_ls_and_prune_report_credentials_without_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let tokens = dir.path().join("custom-credentials");
    let _server = Server::start(&state, Some(&tokens));

    let minted = json(&phux(&state, Some(&tokens), &["pair", "--json"]));
    let id = minted["credential_id"].as_str().unwrap().to_owned();
    let secret = minted["token"].as_str().unwrap().to_owned();

    let listed_output = phux(&state, Some(&tokens), &["pair", "ls", "--json"]);
    let listed = json(&listed_output);
    assert_eq!(listed["operation"], "ls");
    assert_eq!(listed["credentials"].as_array().unwrap().len(), 1);
    assert_eq!(listed["credentials"][0]["id"], id);
    assert_eq!(listed["credentials"][0]["revoked"], false);
    assert!(listed["credentials"][0]["last_seen"].is_null());
    let streams = format!(
        "{}{}",
        String::from_utf8_lossy(&listed_output.stdout),
        String::from_utf8_lossy(&listed_output.stderr)
    );
    assert!(!streams.contains(&secret));

    // Age the credential so prune --unused-for 1h selects it.
    let mut store: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&tokens).unwrap()).unwrap();
    store["credentials"][0]["issued_at"] = serde_json::json!("2000-01-01T00:00:00Z");
    std::fs::write(&tokens, serde_json::to_vec_pretty(&store).unwrap()).unwrap();
    std::fs::set_permissions(&tokens, std::fs::Permissions::from_mode(0o600)).unwrap();

    let pruned_output = phux(
        &state,
        Some(&tokens),
        &["pair", "prune", "--unused-for", "1h", "--json"],
    );
    let pruned = json(&pruned_output);
    assert_eq!(pruned["operation"], "prune");
    assert_eq!(pruned["revoked"], serde_json::json!([id]));
    assert!(!String::from_utf8_lossy(&pruned_output.stdout).contains(&secret));

    let listed = json(&phux(&state, Some(&tokens), &["pair", "ls", "--json"]));
    assert_eq!(listed["credentials"][0]["revoked"], true);
}

#[test]
fn pair_replace_token_revokes_the_previous_bearer() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let tokens = dir.path().join("custom-credentials");
    let _server = Server::start(&state, Some(&tokens));

    let first = json(&phux(&state, Some(&tokens), &["pair", "--json"]));
    let old = first["token"].as_str().unwrap().to_owned();
    let old_id = first["credential_id"].as_str().unwrap().to_owned();

    let second = json(&phux(
        &state,
        Some(&tokens),
        &["pair", "--json", "--replace-token", &old],
    ));
    let new = second["token"].as_str().unwrap().to_owned();
    assert_ne!(old, new);
    assert_ne!(old_id, second["credential_id"].as_str().unwrap());

    let listed = json(&phux(&state, Some(&tokens), &["pair", "ls", "--json"]));
    let rows = listed["credentials"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    let old_row = rows.iter().find(|row| row["id"] == old_id).unwrap();
    assert_eq!(old_row["revoked"], true);
    let new_row = rows
        .iter()
        .find(|row| row["id"] == second["credential_id"])
        .unwrap();
    assert_eq!(new_row["revoked"], false);

    let store = phux_server::auth::TokenStore::load(&tokens).unwrap();
    assert!(!store.verify(&bearer(&old)));
    assert!(store.verify(&bearer(&new)));
}

/// The credentials a store holds, as `phux pair ls --json` lists them (an
/// absent store lists none).
fn credential_count(state: &Path, tokens: &Path) -> usize {
    let listed = json(&phux(state, Some(tokens), &["pair", "ls", "--json"]));
    listed["credentials"].as_array().unwrap().len()
}

/// With no server on the socket, a mint refuses before touching the store:
/// no token, no link, no QR for a door that is not there.
#[test]
fn a_mint_with_no_live_server_mints_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let tokens = dir.path().join("custom-credentials");

    for args in [
        ["pair", "--json"].as_slice(),
        ["pair", "--qr", "--host", "100.64.0.2:8787"].as_slice(),
    ] {
        let out = phux(&state, Some(&tokens), args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success(),
            "`phux {}` must refuse",
            args.join(" ")
        );
        assert!(out.stdout.is_empty(), "a refusal prints no credential");
        assert!(
            stderr.contains("no server is running at")
                && stderr.contains("s.sock")
                && stderr.contains("none was minted")
                && stderr.contains("phux service install"),
            "the refusal names the socket and the remedy; got {stderr:?}"
        );
    }
    assert!(!tokens.exists(), "nothing was written to the store");

    // A stale socket file (a server that exited) is the same refusal.
    std::fs::write(socket_path(&state), b"").unwrap();
    let out = phux(&state, Some(&tokens), &["pair", "--json"]);
    assert!(!out.status.success());
    assert!(!tokens.exists(), "nothing was written to the store");

    // A store that cannot be minted into says so first, in the words
    // `phux host add` answers by migrating.
    std::fs::write(&tokens, format!("{}\n", "ab".repeat(32))).unwrap();
    std::fs::set_permissions(&tokens, std::fs::Permissions::from_mode(0o600)).unwrap();
    let out = phux(&state, Some(&tokens), &["pair", "--json"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("failed to mint token: legacy token store requires explicit migration"),
        "{stderr}"
    );
}

/// Against a live listener the document reports what the server actually
/// bound, and a loopback bind yields no link a device could not use.
#[test]
fn a_mint_reports_the_listener_the_server_bound() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let tokens = dir.path().join("custom-credentials");
    let server = Server::start(&state, Some(&tokens));

    let minted = json(&phux(&state, Some(&tokens), &["pair", "--json"]));
    assert_eq!(minted["ws_addr"], server.wss_addr.as_str());
    assert!(minted["quic_addr"].is_null(), "no QUIC listener: {minted}");
    assert!(
        minted["connect_link"].is_null(),
        "loopback is not dialable from a device: {minted}"
    );
    let store = phux_server::auth::TokenStore::load(&tokens).unwrap();
    assert!(store.verify(&bearer(minted["token"].as_str().unwrap())));
}

/// `--qr` with no address a device can dial is refused before minting; with
/// `--host` over the bound wss listener it renders the link.
#[test]
fn a_qr_mints_only_when_it_can_carry_a_dialable_link() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let tokens = dir.path().join("custom-credentials");
    let server = Server::start(&state, Some(&tokens));

    let refused = phux(&state, Some(&tokens), &["pair", "--qr"]);
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(!refused.status.success(), "stdout={:?}", refused.stdout);
    assert!(refused.stdout.is_empty(), "a refusal prints no credential");
    assert!(
        stderr.contains("--qr needs a connect link")
            && stderr.contains(&server.wss_addr)
            && stderr.contains("--host"),
        "the refusal names the unreachable bind and the remedy; got {stderr:?}"
    );
    assert_eq!(credential_count(&state, &tokens), 0, "nothing was minted");

    let host = format!("wss://{}", server.wss_addr);
    let paired = phux(&state, Some(&tokens), &["pair", "--qr", "--host", &host]);
    let stdout = String::from_utf8_lossy(&paired.stdout);
    assert!(
        paired.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&paired.stderr)
    );
    assert!(
        stdout.contains(&format!("https://phux.sh/connect?url={host}&"))
            && stdout.contains("Scan to pair:"),
        "stdout={stdout}"
    );
    assert_eq!(credential_count(&state, &tokens), 1);
}
