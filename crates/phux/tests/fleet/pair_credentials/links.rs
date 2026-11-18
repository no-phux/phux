//! The links a mint prints: when a QR shows, what it needs, and the phone
//! enrollment ticket every printed link carries (#2397).

use std::os::unix::fs::PermissionsExt;

use super::{Server, bearer, credential_count, phux};

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
    let refused = phux(
        &state,
        Some(&tokens),
        &["pair", "--qr", "--expires", "15m", "--host", &host],
    );
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(!refused.status.success());
    assert!(refused.stdout.is_empty(), "no unusable phone QR is printed");
    assert!(stderr.contains("device-dialable QUIC"), "stderr={stderr}");
    assert_eq!(credential_count(&state, &tokens), 0);

    // A printed link carries a phone enrollment ticket (#2397), which needs
    // the QUIC route too; `--no-enroll` is for a device already enrolled.
    let secret = dir.path().join("pair-secret");
    let mut args = vec!["pair", "--secret-file", secret.to_str().unwrap()];
    args.extend(["--expires", "15m", "--host", &host]);
    let refused = phux(&state, Some(&tokens), &args);
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(!refused.status.success());
    assert!(stderr.contains("--no-enroll"), "stderr={stderr}");
    assert_eq!(credential_count(&state, &tokens), 0);
    args.push("--no-enroll");
    let hidden = phux(&state, Some(&tokens), &args);
    let hidden_out = String::from_utf8_lossy(&hidden.stdout);
    let hidden_err = String::from_utf8_lossy(&hidden.stderr);
    assert!(hidden.status.success(), "stderr={hidden_err}");
    let body = std::fs::read_to_string(&secret).expect("secret file");
    let token = body
        .lines()
        .find_map(|line| line.strip_prefix("token="))
        .expect("token line");
    assert!(!token.is_empty());
    assert!(
        !hidden_out.contains(token),
        "stdout leaked the bearer: {hidden_out}"
    );
    assert!(
        !hidden_err.contains(token),
        "stderr leaked the bearer: {hidden_err}"
    );
    assert!(hidden_out.contains("not shown"));
    assert!(!hidden_out.contains("token="));
    let mode = std::fs::metadata(&secret).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let store = phux_server::auth::TokenStore::load(&tokens).unwrap();
    let admitted = store
        .authenticate(&bearer(token))
        .expect("secret must admit");
    let expiry = admitted.expires_at.expect("expires");
    assert!(expiry > jiff::Timestamp::now() + jiff::SignedDuration::from_mins(14));
    assert_eq!(credential_count(&state, &tokens), 1);
}
