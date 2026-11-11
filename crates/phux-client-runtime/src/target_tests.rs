use super::*;

fn entry(name: &str, endpoint: &str) -> RemoteConfigEntry {
    RemoteConfigEntry {
        name: name.to_owned(),
        endpoint: endpoint.to_owned(),
        ..RemoteConfigEntry::default()
    }
}

fn target(raw: &str) -> RemoteTarget {
    RemoteTarget::parse(raw).unwrap_or_else(|err| unreachable!("{raw}: {err}"))
}

#[test]
fn parses_the_cli_target_spellings() {
    let bare = target("mini");
    assert_eq!(
        (bare.user, bare.host.as_str(), bare.port),
        (None, "mini", None)
    );
    let full = target("phall@mini:9999");
    assert_eq!(full.user.as_deref(), Some("phall"));
    assert_eq!(full.port, Some(9999));
    assert_eq!(full.registry_name(), "phall@mini");
    let bracketed = target("me@[fd7a::1]:8788");
    assert_eq!(bracketed.host, "fd7a::1");
    assert_eq!(bracketed.authority(), "[fd7a::1]:8788");
    let bare_v6 = target("fd7a::1");
    assert_eq!(bare_v6.port, None);
    assert_eq!(bare_v6.authority(), "[fd7a::1]:8788");
    // `mini` and `mini:8788` are one registry host, and a portless
    // target means the ADR-0081 auto-listen port.
    assert_eq!(target("mini:8788").registry_name(), "mini");
    assert_eq!(target("mini").authority(), "mini:8788");
}

#[test]
fn refuses_what_the_cli_refuses() {
    for raw in [
        "",
        "  ",
        "@mini",
        "me@",
        "mini:0",
        "mini:70000",
        "mini:ssh",
        "../evil",
        "#tag",
    ] {
        assert!(RemoteTarget::parse(raw).is_err(), "{raw:?} must be refused");
    }
    let uri = RemoteTarget::parse("quic://mini:8788").expect_err("uri");
    assert!(uri.contains("phux host add"), "{uri}");
}

#[test]
fn matches_name_then_bare_host_then_endpoint_host() {
    let entries = [
        entry("mini", "quic://mini.ts.net:8788"),
        entry("phall@studio", "wss://studio:8787"),
        entry("lab", "quic://lab.example:8788"),
    ];
    assert_eq!(
        find_entry(&entries, &target("phall@studio")).map(|e| e.name.as_str()),
        Some("phall@studio")
    );
    // `host add` registers the bare host; `me@mini` still finds it.
    assert_eq!(
        find_entry(&entries, &target("me@mini")).map(|e| e.name.as_str()),
        Some("mini")
    );
    // Endpoint host is the widest match.
    assert_eq!(
        find_entry(&entries, &target("lab.example")).map(|e| e.name.as_str()),
        Some("lab")
    );
    assert!(find_entry(&entries, &target("elsewhere")).is_none());
}

#[test]
fn explicit_port_overrides_the_dial_but_ssh_is_refused() {
    let (endpoint, transport) = classify("quic://mini:8788", &target("mini:9999")).expect("quic");
    assert_eq!(endpoint, "quic://mini:9999");
    assert_eq!(transport, Transport::Quic("mini:9999".to_owned()));
    let (endpoint, _) = classify("wss://mini:8787", &target("mini:9999")).expect("wss");
    assert_eq!(endpoint, "wss://mini:9999");
    let (untouched, _) = classify("quic://mini:9999", &target("mini")).expect("portless");
    assert_eq!(untouched, "quic://mini:9999");
    let (_, ssh) = classify("ssh://me@mini", &target("mini")).expect("ssh");
    assert_eq!(ssh, Transport::ssh("ssh://me@mini"));
    assert!(classify("http://mini", &target("mini")).is_err());
}

#[test]
fn resolves_through_the_cli_registry_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let token = dir.path().join("mini.token");
    std::fs::write(&token, "# minted by phux pair\n\n  abcd01  \n").expect("token");
    let config = dir.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            "[[remote]]\nname = \"mini\"\nendpoint = \"quic://127.0.0.1:8788\"\n\
             token-file = \"{}\"\ncert-fingerprint = \"{}\"\nsession = \"work\"\n",
            token.display(),
            "ab".repeat(32)
        ),
    )
    .expect("config");
    let resolved = resolve("me@mini", Some(&config)).expect("resolve");
    assert_eq!(resolved.name, "mini");
    assert_eq!(resolved.endpoint, "quic://127.0.0.1:8788");
    assert_eq!(resolved.session.as_deref(), Some("work"));
    assert_eq!(resolved.tls_server_name, None);
    // Only the path: resolving for display never reads the secret.
    assert_eq!(resolved.token_file.as_deref(), Some(token.as_path()));
    assert!(!format!("{resolved:?}").contains("abcd01"));
    std::fs::remove_file(&token).expect("remove token");
    assert!(
        resolve("me@mini", Some(&config)).is_ok(),
        "a missing token file is the dial's problem, not resolution's"
    );

    let missing = resolve("studio", Some(&config)).expect_err("unregistered");
    assert!(missing.contains("phux --remote studio"), "{missing}");
    // An absent config file is an empty registry, not an I/O failure.
    let empty = resolve("mini", Some(&dir.path().join("absent.toml"))).expect_err("empty");
    assert!(empty.contains("not a registered host"), "{empty}");
}

#[test]
fn resolves_layered_remotes_without_reading_tui_tables() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("shared.toml"),
        "[[remote]]\nname = 'mini'\nendpoint = 'quic://127.0.0.1:8788'\n",
    )
    .expect("shared registry");
    let config = dir.path().join("config.toml");
    std::fs::write(
        &config,
        "extends = ['shared.toml']\n[keybindings]\nprefix = 42\n",
    )
    .expect("config with invalid TUI schema");
    let resolved = resolve("mini", Some(&config)).expect("client-neutral resolution");
    assert_eq!(resolved.name, "mini");
    assert_eq!(resolved.endpoint, "quic://127.0.0.1:8788");
}

#[test]
fn a_relay_routed_entry_resolves_with_its_tls_server_name() {
    let mut routed = entry("mini", "quic://relay.example:4433");
    routed.tls_server_name = Some("mini-route".to_owned());
    let resolved = resolve_entry(&routed, &target("mini")).expect("resolve");
    assert_eq!(
        resolved.transport,
        Transport::Quic("relay.example:4433".to_owned())
    );
    assert_eq!(resolved.tls_server_name.as_deref(), Some("mini-route"));

    routed.tls_server_name = Some("  ".to_owned());
    let blank = resolve_entry(&routed, &target("mini")).expect("resolve");
    assert_eq!(blank.tls_server_name, None, "a blank name is no override");
}

#[test]
fn tls_server_names_are_dns_names() {
    for ok in ["mini-route", "a", " relay.example.com ", "x1.y-2.z"] {
        assert_eq!(
            validate_tls_server_name(ok).as_deref(),
            Ok(ok.trim()),
            "{ok:?}"
        );
    }
    for bad in [
        "",
        "-route",
        "route-",
        "a..b",
        "has space",
        "slash/route",
        "127.0.0.1",
        "::1",
        &"a".repeat(64),
    ] {
        assert!(validate_tls_server_name(bad).is_err(), "{bad:?}");
    }
}

/// The registry's enrolled client certificate is what the dial presents;
/// an entry without one presents none, and half of one is refused.
#[test]
fn resolution_carries_the_enrolled_client_identity() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("config.toml");
    let pin = "ab".repeat(32);
    std::fs::write(
        &config,
        format!(
            "[[remote]]\nname = \"mini\"\nendpoint = \"quic://127.0.0.1:8788\"\n\
             cert-fingerprint = \"{pin}\"\nclient-cert = \"/s/mini.pem\"\n\
             client-key = \"/s/mini.key\"\n\
             [[remote]]\nname = \"bare\"\nendpoint = \"quic://127.0.0.1:8789\"\n\
             [[remote]]\nname = \"half\"\nendpoint = \"quic://127.0.0.1:8790\"\n\
             client-cert = \"/s/half.pem\"\n"
        ),
    )
    .expect("config");
    let resolved = resolve("mini", Some(&config)).expect("mini");
    assert_eq!(
        resolved.client_identity,
        TlsClientIdentity::PemFiles {
            certificate: PathBuf::from("/s/mini.pem"),
            private_key: PathBuf::from("/s/mini.key"),
        }
    );
    let target = crate::connection::Target::from(resolved);
    assert!(matches!(
        target.client_identity,
        TlsClientIdentity::PemFiles { .. }
    ));
    assert_eq!(
        resolve("bare", Some(&config))
            .expect("bare")
            .client_identity,
        TlsClientIdentity::None
    );
    let half = resolve("half", Some(&config)).expect_err("half");
    assert!(half.contains("together"), "{half}");
}

/// An entry with only a leaf pin dials that leaf and learns nothing
/// (ADR-0179); a CA pinned at pairing in `known-authorities` replaces
/// the leaf, and the registry itself is never rewritten.
#[test]
fn a_leaf_pinned_entry_learns_nothing_and_a_paired_authority_wins() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("config.toml");
    let pin = "AB:".repeat(31) + "AB";
    let registry = format!(
        "[[remote]]\nname = \"mini\"\nendpoint = \"quic://127.0.0.1:8788\"\n\
         cert-fingerprint = \"{pin}\"\n"
    );
    std::fs::write(&config, &registry).expect("config");
    let first = resolve("mini", Some(&config)).expect("mini");
    assert_eq!(first.authority.ca, None);
    assert_eq!(
        first.authority.trust(first.cert_fingerprint.as_deref()),
        Some(phux_dial::CertTrust::Pinned(pin.clone()))
    );

    let authority = format!("sha256:{}", "c".repeat(64));
    let store = phux_config_core::known_authorities::path_beside(&config);
    phux_config_core::known_authorities::record(&store, &pin, None, &authority)
        .expect("pin at pairing");
    let second = resolve("mini", Some(&config)).expect("mini");
    assert_eq!(second.authority.ca.as_deref(), Some(authority.as_str()));
    assert_eq!(
        second.authority.trust(second.cert_fingerprint.as_deref()),
        Some(phux_dial::CertTrust::Authority { ca: authority })
    );
    assert_eq!(
        std::fs::read_to_string(&config).expect("config"),
        registry,
        "the registry is untouched"
    );
}

#[test]
fn duplicate_names_are_ambiguous_not_first_wins() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("config.toml");
    std::fs::write(
        &config,
        "[[remote]]\nname = \"mini\"\nendpoint = \"ws://127.0.0.1:1\"\n\
         [[remote]]\nname = \"mini\"\nendpoint = \"ws://127.0.0.1:2\"\n",
    )
    .expect("config");
    let err = resolve("mini", Some(&config)).expect_err("duplicate");
    assert!(err.contains("twice"), "{err}");
}

#[test]
fn unreadable_token_names_the_path_and_never_the_bytes() {
    let err = read_token(Some(Path::new("/nonexistent/phux/token"))).expect_err("missing");
    assert!(err.contains("/nonexistent/phux/token"), "{err}");
    assert_eq!(read_token(None), Ok(None));
}

#[test]
fn a_paused_entry_is_not_resolved() {
    let mut paused = entry("mini", "quic://mini:8788");
    paused.paused = true;
    let target = RemoteTarget::parse("mini").expect("target");
    let refusal = resolve_entry(&paused, &target).expect_err("paused");
    assert!(refusal.contains("phux host enable mini"), "{refusal}");
}
