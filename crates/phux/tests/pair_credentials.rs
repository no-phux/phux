//! Real-CLI credential lifecycle and custom-store integrity coverage.

#![allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]

use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Output};

const PHUX: &str = env!("CARGO_BIN_EXE_phux");

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
/// because `PATH` and friends still have to reach the child.
fn phux(state: &std::path::Path, tokens: Option<&std::path::Path>, args: &[&str]) -> Output {
    let mut command = Command::new(PHUX);
    command
        .env("XDG_STATE_HOME", state)
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
    encoded
        .as_bytes()
        .chunks_exact(2)
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
