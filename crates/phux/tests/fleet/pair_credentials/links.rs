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
    assert!(expiry > chrono::Utc::now() + chrono::Duration::minutes(14));
    assert_eq!(credential_count(&state, &tokens), 1);
}

/// A bound QUIC listener on loopback is still not an enrollment route for a phone.
#[test]
fn a_phone_qr_refuses_an_unreachable_quic_listener_before_minting() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let tokens = dir.path().join("custom-credentials");
    let server = Server::start_with_quic(&state, Some(&tokens), true);
    let host = format!("wss://{}", server.wss_addr);
    let refused = phux(&state, Some(&tokens), &["pair", "--qr", "--host", &host]);
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(!refused.status.success(), "stderr={stderr}");
    assert!(stderr.contains("device-dialable QUIC"), "stderr={stderr}");
    assert!(refused.stdout.is_empty());
    assert_eq!(credential_count(&state, &tokens), 0);
}

/// The QR is the default only on a terminal: piped output keeps the link as
/// text, a default mint with no link is not refused the way `--qr` is, and
/// `--no-qr` neither combines with `--qr` nor with a lifecycle action.
#[test]
fn the_qr_defaults_on_only_for_a_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let tokens = dir.path().join("custom-credentials");
    let server = Server::start(&state, Some(&tokens));

    // No link (loopback bind) and no `--qr`: the mint goes ahead, QR-less.
    let plain = phux(&state, Some(&tokens), &["pair"]);
    let stdout = String::from_utf8_lossy(&plain.stdout);
    assert!(
        plain.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&plain.stderr)
    );
    assert!(!stdout.contains("Scan to pair:"), "stdout={stdout}");

    // A link printed into a pipe stays text; `--no-qr` says so explicitly.
    let host = format!("wss://{}", server.wss_addr);
    for args in [
        ["pair", "--no-enroll", "--host", host.as_str()].as_slice(),
        ["pair", "--no-qr", "--no-enroll", "--host", host.as_str()].as_slice(),
    ] {
        let out = phux(&state, Some(&tokens), args);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "`phux {}`", args.join(" "));
        assert!(
            stdout.contains(&format!("https://phux.sh/connect?url={host}&"))
                && !stdout.contains("Scan to pair:"),
            "`phux {}`: stdout={stdout}",
            args.join(" ")
        );
    }
    assert_eq!(credential_count(&state, &tokens), 3);

    for (args, needle) in [
        (["pair", "--qr", "--no-qr"].as_slice(), "--no-qr"),
        (
            ["pair", "--no-qr", "ls"].as_slice(),
            "--no-qr belongs to minting",
        ),
    ] {
        let out = phux(&state, Some(&tokens), args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "`phux {}`", args.join(" "));
        assert!(
            stderr.contains(needle),
            "`phux {}`: {stderr}",
            args.join(" ")
        );
    }
    assert_eq!(
        credential_count(&state, &tokens),
        3,
        "usage errors mint nothing"
    );
}
