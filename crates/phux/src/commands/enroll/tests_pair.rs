//! Unit tests for the pair document, endpoint candidates, ssh parsing, and
//! the pairing token.

use super::pair::{PairReport, candidate_endpoints, token_path, write_token};
use super::ssh::{authority, destination_host, parse_ssh_g_hostname};
use std::path::Path;

pub(super) fn report(fp: Option<&str>, overlay: &[&str]) -> PairReport {
    PairReport {
        ca_fingerprint: None,
        token: "deadbeef".to_owned(),
        cert_fingerprint: fp.map(str::to_owned),
        overlay_addresses: overlay.iter().map(|a| (*a).to_owned()).collect(),
    }
}

#[test]
fn parses_the_pair_document() {
    let parsed = PairReport::parse(
        r#"{
              "token": "abc123",
              "cert_fingerprint": "AB:CD",
              "overlay_addresses": ["100.64.0.2"],
              "ws_addr": null,
              "quic_addr": "0.0.0.0:8788"
            }"#,
    )
    .expect("parse");
    assert_eq!(parsed.token, "abc123");
    assert_eq!(parsed.cert_fingerprint.as_deref(), Some("AB:CD"));
    assert_eq!(
        parsed.ca_fingerprint, None,
        "a far phux that predates ADR-0153"
    );
    assert_eq!(parsed.overlay_addresses, vec!["100.64.0.2".to_owned()]);
}

/// ADR-0153: the far server's CA is pinned in its canonical spelling, and
/// a malformed one pins nothing rather than failing the pairing.
#[test]
fn the_pair_document_names_the_certificate_authority() {
    let authority = format!("sha256:{}", "cd".repeat(32));
    let parsed = PairReport::parse(&format!(
        r#"{{"token":"t","cert_fingerprint":"AB","ca_fingerprint":"{}"}}"#,
        authority.to_uppercase().replace("SHA256", "sha256")
    ))
    .expect("parse");
    assert_eq!(parsed.ca_fingerprint, Some(authority));
    let malformed =
        PairReport::parse(r#"{"token":"t","ca_fingerprint":"sha256:abc"}"#).expect("parse");
    assert_eq!(malformed.ca_fingerprint, None);
}

#[test]
fn a_pretty_printed_document_with_a_multi_line_array_is_read_whole() {
    // Pretty-printed output puts an overlay address alone on a line as a bare
    // JSON string; it must not be mistaken for the document.
    let stdout = "Last login: never\n{\n  \"cert_fingerprint\": \"AB:CD\",\n  \"overlay_addresses\": [\n    \"100.79.155.27\"\n  ],\n  \"quic_addr\": null,\n  \"schema_version\": 1,\n  \"token\": \"863e\",\n  \"ws_addr\": \":8787\"\n}\n";
    let parsed = PairReport::parse(stdout).expect("parse");
    assert_eq!(parsed.token, "863e");
    assert_eq!(parsed.cert_fingerprint.as_deref(), Some("AB:CD"));
    assert_eq!(parsed.overlay_addresses, vec!["100.79.155.27".to_owned()]);
}

#[test]
fn tolerates_unknown_fields_from_a_newer_remote() {
    // A newer remote phux must be able to add keys without breaking an
    // older `phux host add`.
    let parsed = PairReport::parse(r#"{"token":"t","brand_new_field":42}"#).expect("parse");
    assert_eq!(parsed.token, "t");
    assert_eq!(parsed.cert_fingerprint, None);
    assert!(parsed.overlay_addresses.is_empty());
}

#[test]
fn the_document_is_found_past_startup_noise() {
    // A shell startup file that prints on a non-interactive login must
    // not hide the one-line document.
    let parsed = PairReport::parse("Welcome to box\n{\"token\":\"t\"}\n\n").expect("parse");
    assert_eq!(parsed.token, "t");
}

#[test]
fn refuses_a_document_with_no_token() {
    assert!(PairReport::parse(r#"{"cert_fingerprint":"AB"}"#).is_err());
    assert!(PairReport::parse(r#"{"token":""}"#).is_err());
    // Not JSON at all — the shape of an ssh banner leaking into stdout.
    assert!(PairReport::parse("Welcome to Ubuntu\n").is_err());
}

#[test]
fn candidates_are_overlay_addresses_then_the_ssh_host_when_pinned() {
    assert_eq!(
        candidate_endpoints("mini.lan", &report(Some("AB"), &["100.64.0.2"]), None, 8788),
        vec![
            "quic://100.64.0.2:8788".to_owned(),
            "quic://mini.lan:8788".to_owned()
        ]
    );
    // The ssh host is not listed twice when it is the overlay address.
    assert_eq!(
        candidate_endpoints(
            "100.64.0.2",
            &report(Some("AB"), &["100.64.0.2"]),
            None,
            8788
        ),
        vec!["quic://100.64.0.2:8788".to_owned()]
    );
    // No overlay address: the ssh host alone is still worth a dial.
    assert_eq!(
        candidate_endpoints("mini", &report(Some("AB"), &[]), None, 8788),
        vec!["quic://mini:8788".to_owned()]
    );
}

#[test]
fn nothing_is_dialable_without_a_pin() {
    // ADR-0031 would refuse the dial, so registering it would only move
    // the failure later.
    assert!(candidate_endpoints("mini", &report(None, &["100.64.0.2"]), None, 8788).is_empty());
}

#[test]
fn operator_endpoint_override_is_the_only_candidate() {
    let report = report(Some("AB"), &["100.64.0.2"]);
    assert_eq!(
        candidate_endpoints("mini", &report, Some("mini.ts.net:9999"), 8788),
        vec!["quic://mini.ts.net:9999".to_owned()]
    );
    // A full URI passes through, so wss:// is reachable this way.
    assert_eq!(
        candidate_endpoints("mini", &report, Some("wss://mini.ts.net:8787"), 8788),
        vec!["wss://mini.ts.net:8787".to_owned()]
    );
}

#[test]
fn ipv6_overlay_address_is_bracketed() {
    // Without brackets the result would not parse as HOST:PORT.
    assert_eq!(
        candidate_endpoints("", &report(Some("AB"), &["fd7a::1"]), None, 8788),
        vec!["quic://[fd7a::1]:8788".to_owned()]
    );
}

#[test]
fn ssh_g_names_the_real_host() {
    let output = "user me\nhostname 10.0.0.5\nport 22\n";
    assert_eq!(parse_ssh_g_hostname(output).as_deref(), Some("10.0.0.5"));
    assert_eq!(parse_ssh_g_hostname("user me\n"), None);
}

#[test]
fn destination_host_reads_every_ssh_spelling() {
    assert_eq!(destination_host("box"), "box");
    assert_eq!(destination_host("me@box"), "box");
    assert_eq!(destination_host("me@box:2222"), "box");
    assert_eq!(destination_host("ssh://me@box:2222"), "box");
    assert_eq!(destination_host("ssh://me@[fd7a::1]:2222"), "fd7a::1");
    assert_eq!(destination_host("fd7a::1"), "fd7a::1");
}

#[test]
fn authority_brackets_ipv6() {
    assert_eq!(authority("box", 60123), "box:60123");
    assert_eq!(authority("fd7a::1", 60123), "[fd7a::1]:60123");
}

#[test]
fn token_is_written_owner_only() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().expect("tempdir");
    let path = token_path(dir.path(), "mini");
    write_token(&path, "deadbeef").expect("write");

    assert_eq!(std::fs::read_to_string(&path).expect("read"), "deadbeef\n");
    let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "a bearer token must not be readable");
}

#[test]
fn token_path_is_namespaced_per_remote() {
    let state = Path::new("/state");
    assert_eq!(
        token_path(state, "mini"),
        Path::new("/state/remotes/mini.token")
    );
    assert_ne!(token_path(state, "mini"), token_path(state, "studio"));
}
