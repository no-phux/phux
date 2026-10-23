//! Unit tests for the `[[remote]]` registry.

use super::{Endpoint, NewRemote, RemoteEntry, read_token, validate_name, validate_token_file};
use std::path::{Path, PathBuf};

fn entry_with_token(path: Option<PathBuf>) -> RemoteEntry {
    RemoteEntry {
        index: 0,
        name: "mini".to_owned(),
        endpoint: "quic://mini:8788".to_owned(),
        token_file: path,
        cert_fingerprint: None,
        tls_server_name: None,
        session: None,
        ssh: None,
        direct: None,
        remote_phux: None,
        client_cert: None,
        client_key: None,
    }
}

#[test]
fn endpoint_parse_covers_the_three_schemes() {
    assert_eq!(
        Endpoint::parse("quic://mini.ts.net:8788"),
        Ok(Endpoint::Quic("mini.ts.net:8788".to_owned()))
    );
    assert_eq!(
        Endpoint::parse("wss://mini.ts.net:8787"),
        Ok(Endpoint::Ws("wss://mini.ts.net:8787".to_owned()))
    );
    assert_eq!(
        Endpoint::parse("ssh://mini"),
        Ok(Endpoint::Ssh("mini".to_owned()))
    );
    // Whitespace from a paste is trimmed, not rejected.
    assert_eq!(
        Endpoint::parse("  ssh://mini  "),
        Ok(Endpoint::Ssh("mini".to_owned()))
    );
}

#[test]
fn endpoint_parse_rejects_typos_at_registration_time() {
    assert!(Endpoint::parse("mini:8788").is_err(), "bare host:port");
    assert!(Endpoint::parse("https://mini").is_err(), "wrong scheme");
    assert!(Endpoint::parse("quic://mini").is_err(), "quic needs a port");
    assert!(Endpoint::parse("ssh://").is_err(), "ssh needs a host");
}

#[test]
fn ssh_needs_no_pairing_but_quic_and_wss_do() {
    assert!(!Endpoint::parse("ssh://mini").expect("ssh").needs_pairing());
    assert!(
        Endpoint::parse("quic://mini:1")
            .expect("quic")
            .needs_pairing()
    );
    assert!(
        Endpoint::parse("wss://mini:1")
            .expect("wss")
            .needs_pairing()
    );
}

/// An enrolled identity is two absolute paths or nothing; half of one
/// is refused at registration and at dial time alike.
#[test]
fn a_client_identity_is_whole_and_absolute() {
    let fp = "ab".repeat(32);
    let new = || NewRemote::new("mini", "quic://mini:8788", None, Some(&fp), None);
    let cert = Path::new("/state/remotes/mini.client.pem");
    let key = Path::new("/state/remotes/mini.client.key");
    let whole = new()
        .expect("entry")
        .with_client_identity(Some((cert, key)))
        .expect("whole identity");
    assert_eq!(whole.client_cert.as_deref(), Some(cert));
    assert_eq!(whole.client_key.as_deref(), Some(key));
    let none = new()
        .expect("entry")
        .with_client_identity(None)
        .expect("no identity");
    assert!(none.client_cert.is_none() && none.client_key.is_none());
    assert!(
        new()
            .expect("entry")
            .with_client_identity(Some((Path::new("rel.pem"), key)))
            .is_err()
    );

    let mut entry = entry_with_token(None);
    assert_eq!(entry.client_identity(), Ok(None));
    entry.client_cert = Some(cert.to_path_buf());
    let half = entry.client_identity().expect_err("half an identity");
    assert!(half.contains("together"), "{half}");
    entry.client_key = Some(key.to_path_buf());
    assert!(matches!(
        entry.client_identity(),
        Ok(Some(
            phux_dial::TlsClientIdentity::PemFiles { .. }
                | phux_dial::TlsClientIdentity::RequirePaired { .. }
        ))
    ));
}

#[test]
fn paired_transports_fail_closed_without_a_pin() {
    // ADR-0038's posture: refuse at registration, where the operator can
    // still see what they pasted.
    let err = NewRemote::new("mini", "quic://mini:8788", None, None, None)
        .expect_err("unpinned quic must be refused");
    assert!(err.contains("--cert-fingerprint"), "got {err}");

    // ssh:// rides existing ssh trust, so it needs no pin.
    assert!(NewRemote::new("mini", "ssh://mini", None, None, None).is_ok());

    // With a pin, quic is accepted.
    let fp = "ab".repeat(32);
    assert!(NewRemote::new("mini", "quic://mini:8788", None, Some(&fp), None).is_ok());
}

/// A relay route (ADR-0149) is a TLS server name on a TLS endpoint:
/// validated as a DNS name, refused on `ssh://`, and blank clears it.
#[test]
fn tls_server_name_is_a_dns_name_on_a_tls_endpoint() {
    let fp = "ab".repeat(32);
    let relay = || NewRemote::new("mini", "quic://relay:4433", None, Some(&fp), None);
    let routed = relay()
        .expect("quic entry")
        .with_tls_server_name(Some(" mini-route "))
        .expect("a route is a DNS label");
    assert_eq!(routed.tls_server_name.as_deref(), Some("mini-route"));
    assert!(
        relay()
            .expect("quic entry")
            .with_tls_server_name(Some("not a name"))
            .is_err()
    );
    let cleared = routed
        .with_tls_server_name(Some(" "))
        .expect("blank clears");
    assert_eq!(cleared.tls_server_name, None);
    let err = NewRemote::new("mini", "ssh://mini", None, None, None)
        .expect("ssh entry")
        .with_tls_server_name(Some("mini-route"))
        .expect_err("ssh has no TLS server name");
    assert!(err.contains("quic://"), "got {err}");
}

/// `direct` only means something beside an `ssh://` route: it is the
/// paired endpoint a later attach promotes. Beside a direct endpoint it
/// would only duplicate it, so it is dropped rather than stored.
#[test]
fn only_a_phux_other_than_the_default_is_recorded() {
    let new = || NewRemote::new("mini", "ssh://me@mini", None, None, None).expect("entry");
    for (given, kept) in [
        (None, None),
        (Some("phux"), None),
        (Some("  "), None),
        (
            Some(" /opt/homebrew/bin/phux "),
            Some("/opt/homebrew/bin/phux"),
        ),
    ] {
        assert_eq!(
            new().with_remote_phux(given).remote_phux.as_deref(),
            kept,
            "{given:?}"
        );
    }
    let mut entry = entry_with_token(None);
    entry.endpoint = "ssh://me@mini".to_owned();
    assert_eq!(entry.remote_phux(), "phux");
    entry.remote_phux = Some("/home/me/.local/bin/phux".to_owned());
    assert_eq!(entry.remote_phux(), "/home/me/.local/bin/phux");
    let carried = NewRemote::from_entry(&entry).expect("carried");
    assert_eq!(
        carried.remote_phux.as_deref(),
        Some("/home/me/.local/bin/phux"),
        "a rewrite (renew, upgrade) keeps it"
    );
}

#[test]
fn direct_candidate_is_kept_only_beside_an_ssh_route() {
    let fp = "ab".repeat(32);
    let ssh = NewRemote::new("mini", "ssh://me@mini", None, Some(&fp), None)
        .expect("ssh entry")
        .with_direct(Some("quic://100.64.0.7:8788"))
        .expect("a quic candidate is valid");
    assert_eq!(ssh.direct.as_deref(), Some("quic://100.64.0.7:8788"));

    let quic = NewRemote::new("mini", "quic://mini:8788", None, Some(&fp), None)
        .expect("quic entry")
        .with_direct(Some("quic://100.64.0.7:8788"))
        .expect("dropped, not refused");
    assert_eq!(quic.direct, None, "already direct: nothing to promote");

    let err = NewRemote::new("mini", "ssh://mini", None, None, None)
        .expect("ssh entry")
        .with_direct(Some("ssh://mini"))
        .expect_err("an ssh candidate is not a direct route");
    assert!(err.contains("quic://"), "got {err}");
    assert!(
        NewRemote::new("mini", "ssh://mini", None, None, None)
            .expect("ssh entry")
            .with_direct(Some("nonsense"))
            .is_err(),
        "a candidate is validated like an endpoint"
    );
}

/// Beside a `wss://` route, only a `quic://` candidate is kept: it is the
/// upgrade a later attach promotes, and anything else is no upgrade.
#[test]
fn a_wss_route_keeps_only_a_quic_candidate() {
    let fp = "ab".repeat(32);
    let wss = || NewRemote::new("mini", "wss://100.64.0.7:8787", None, Some(&fp), None);
    let kept = wss()
        .expect("wss entry")
        .with_direct(Some("quic://100.64.0.7:8788"))
        .expect("a quic candidate is valid");
    assert_eq!(kept.direct.as_deref(), Some("quic://100.64.0.7:8788"));
    let sideways = wss()
        .expect("wss entry")
        .with_direct(Some("wss://100.64.0.7:9999"))
        .expect("dropped, not refused");
    assert_eq!(sideways.direct, None);
}

/// The destination a repair sshes to: the enrolled one first, then the
/// host of an `ssh://` endpoint, then the name itself.
#[test]
fn ssh_destination_prefers_what_the_entry_remembers() {
    let mut entry = entry_with_token(None);
    assert_eq!(
        entry.ssh_destination(),
        "mini",
        "a bare name is an ssh alias"
    );
    entry.endpoint = "ssh://me@box".to_owned();
    assert_eq!(entry.ssh_destination(), "me@box");
    entry.ssh = Some("me@mini.lan".to_owned());
    assert_eq!(entry.ssh_destination(), "me@mini.lan");
}

#[test]
fn names_must_not_collide_with_the_selector_grammar() {
    // `phux attach @3` already means a terminal id; a remote named `@3`
    // would make that ambiguous.
    for bad in ["@id", "#tag", ".", "=", "a/b", "a:b", "", "  "] {
        assert!(validate_name(bad).is_err(), "{bad:?} must be rejected");
    }
    assert_eq!(validate_name("  mini  ").as_deref(), Ok("mini"));
    assert_eq!(
        validate_name("studio-mini_2").as_deref(),
        Ok("studio-mini_2")
    );
}

#[test]
fn token_file_must_be_absolute() {
    // A relative path resolves against whatever cwd `phux attach` ran
    // in, which is not where enrollment wrote the token.
    assert!(validate_token_file(Path::new("tokens")).is_err());
    assert!(validate_token_file(Path::new("")).is_err());
    assert!(validate_token_file(Path::new("/abs/tokens")).is_ok());
}

#[test]
fn read_token_takes_the_first_meaningful_line() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("token");
    std::fs::write(&path, "# a comment\n\n  deadbeef  \nsecond\n").expect("write");
    assert_eq!(
        read_token(&entry_with_token(Some(path))).expect("read"),
        Some("deadbeef".to_owned())
    );
}

#[test]
fn read_token_is_none_without_a_token_file() {
    assert_eq!(read_token(&entry_with_token(None)).expect("read"), None);
}

#[test]
fn read_token_names_the_path_it_could_not_read() {
    let err = read_token(&entry_with_token(Some(PathBuf::from("/nope/missing"))))
        .expect_err("missing file must error");
    assert!(err.contains("/nope/missing"), "got {err}");

    let dir = tempfile::tempdir().expect("tempdir");
    let empty = dir.path().join("empty");
    std::fs::write(&empty, "# only a comment\n").expect("write");
    let err = read_token(&entry_with_token(Some(empty))).expect_err("no token line");
    assert!(err.contains("no token line"), "got {err}");
}
