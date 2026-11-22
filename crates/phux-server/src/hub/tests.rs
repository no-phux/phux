//! Hub table and runtime tests.
use super::*;

#[tokio::test(flavor = "current_thread")]
async fn a_doorbell_promotes_a_non_hub_and_leaves_an_empty_registry_alone() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let idle = crate::state::SharedState::new();
            let idle_cancel = tokio_util::sync::CancellationToken::new();
            idle.with_mut(|s| {
                s.federation_mut().arm_reload(
                    Some(SatelliteSource::new(|| Ok(Vec::new()))),
                    idle_cancel.clone(),
                );
            });
            reload_satellites(&idle);
            assert!(idle.with(|s| !s.federation().is_hub()));
            idle_cancel.cancel();

            let state = crate::state::SharedState::new();
            let cancel = tokio_util::sync::CancellationToken::new();
            state.with_mut(|s| {
                s.federation_mut().arm_reload(
                    Some(SatelliteSource::new(|| {
                        Ok(vec![entry("edge", "quic://127.0.0.1:9", true)])
                    })),
                    cancel.clone(),
                );
            });
            reload_satellites(&state);
            let host = state.with(|s| {
                let table = s.federation().table().expect("promoted to a hub");
                assert_eq!(table.len(), 1);
                table.iter().next().expect("satellite").0.clone()
            });
            assert!(state.with(|s| s.federation().relay(&host).is_some()));
            cancel.cancel();
        })
        .await;
}

fn entry(name: &str, endpoint: &str, enabled: bool) -> SatelliteConfigEntry {
    SatelliteConfigEntry {
        name: name.to_owned(),
        endpoint: endpoint.to_owned(),
        enabled,
        token_file: None,
        cert_fingerprint: None,
        client_cert: None,
        client_key: None,
        color: None,
    }
}

#[test]
fn parses_quic_ws_and_inbound_endpoints_and_displays_them_back() {
    let quic = |host: &str| SatelliteTarget::Quic {
        host: host.to_owned(),
        port: 8788,
    };
    let url = |url: &str| url.to_owned();
    assert_eq!(parse_endpoint("quic://devbox:8788"), Ok(quic("devbox")));
    assert_eq!(parse_endpoint("quic://[::1]:8788"), Ok(quic("[::1]")));
    let ws = "ws://127.0.0.1:8787";
    assert_eq!(parse_endpoint(ws), Ok(SatelliteTarget::Ws { url: url(ws) }));
    let wss = "wss://host:8787/phux";
    assert_eq!(
        parse_endpoint(wss),
        Ok(SatelliteTarget::Wss { url: url(wss) })
    );
    // `inbound://` is exactly the bare scheme.
    assert_eq!(parse_endpoint("inbound://"), Ok(SatelliteTarget::Inbound));
    for endpoint in ["inbound://host", "inbound:", "inbound://:1"] {
        assert!(parse_endpoint(endpoint).is_err(), "{endpoint}");
    }
    for endpoint in [
        "quic://devbox:8788",
        "ssh://devbox",
        "ssh://me@devbox:2222",
        "ssh://[::1]:2222",
        "inbound://",
    ] {
        let shown = parse_endpoint(endpoint).unwrap().to_string();
        assert_eq!(shown, endpoint);
    }
}

#[test]
fn parses_ssh_host_user_port_matrix() {
    let cases: &[(&str, Option<&str>, &str, Option<u16>)] = &[
        ("ssh://devbox", None, "devbox", None),
        ("ssh://devbox.example.com", None, "devbox.example.com", None),
        ("ssh://devbox:2222", None, "devbox", Some(2222)),
        ("ssh://me@devbox", Some("me"), "devbox", None),
        ("ssh://me@devbox:2222", Some("me"), "devbox", Some(2222)),
        (
            "ssh://build-agent@10.0.0.7:22",
            Some("build-agent"),
            "10.0.0.7",
            Some(22),
        ),
        ("ssh://[::1]", None, "::1", None),
        ("ssh://[::1]:2222", None, "::1", Some(2222)),
        (
            "ssh://me@[2001:db8::1]:2222",
            Some("me"),
            "2001:db8::1",
            Some(2222),
        ),
    ];
    for (endpoint, user, host, port) in cases {
        assert_eq!(
            parse_endpoint(endpoint),
            Ok(SatelliteTarget::Ssh {
                user: user.map(str::to_owned),
                host: (*host).to_owned(),
                port: *port,
            }),
            "{endpoint}"
        );
    }
}

#[test]
fn rejects_malformed_endpoints() {
    // (endpoint, expected reason fragment; "" = any error)
    let cases: &[(&str, &str)] = &[
        ("devbox:8788", "missing '<scheme>://'"),
        ("://devbox", "empty scheme"),
        ("http://devbox", "unsupported scheme \"http\""),
        ("quic://", ""),
        ("quic://:8788", ""),
        ("ws:///path", ""),
        ("wss://", ""),
        ("quic://devbox", "explicit port"),
        ("quic://devbox:phux", ""),
        ("quic://devbox:0", ""),
        ("quic://devbox:70000", ""),
        ("quic://devbox:8788/route", "no path"),
        ("ssh://devbox/path", "no path"),
        ("ssh://@devbox", "empty user"),
        ("ssh://me@", "empty host"),
        ("ssh://devbox:", "not a valid port"),
        ("ssh://devbox:0", "non-zero"),
        ("ssh://devbox:70000", "not a valid port"),
        ("ssh://devbox:ssh", "not a valid port"),
        ("ssh://[::1", "unclosed"),
        ("ssh://[]", "empty host"),
        ("ssh://[::1]junk", "trailing garbage"),
        // Unbracketed IPv6 is ambiguous with `host:port`.
        ("ssh://::1", "invalid character"),
        // Option injection and shell metacharacters never reach argv.
        ("ssh://-oProxyCommand=evil", "must not start with '-'"),
        ("ssh://-fool@devbox", "must not start with '-'"),
        ("ssh://dev box", "invalid character"),
        ("ssh://devbox;rm", "invalid character"),
        ("ssh://me$@devbox", "invalid character"),
        ("ssh://dev`box`", "invalid character"),
    ];
    for (endpoint, fragment) in cases {
        let err = parse_endpoint(endpoint).unwrap_err();
        assert!(err.contains(fragment), "{endpoint}: {err}");
    }
}

// --- table construction ------------------------------------------

#[test]
fn builds_table_keyed_by_host_with_auth_material_skipping_disabled_entries() {
    let mut devbox = entry("devbox", "quic://devbox:8788", true);
    devbox.token_file = Some(PathBuf::from("/secrets/devbox.token"));
    devbox.cert_fingerprint = Some("AB:CD".to_owned());
    let table = HubTable::from_registry(&[
        devbox,
        entry("sandbox", "wss://sandbox:8787", true),
        // Skipped even though malformed.
        entry("parked", "not a uri at all", false),
    ])
    .unwrap();
    assert_eq!(table.len(), 2);
    let held = table.get(&SatelliteHost::new("devbox")).unwrap();
    let quic = SatelliteTarget::Quic {
        host: "devbox".to_owned(),
        port: 8788,
    };
    assert_eq!(held.target, quic);
    assert_eq!(
        held.token_file.as_deref(),
        Some(std::path::Path::new("/secrets/devbox.token"))
    );
    assert_eq!(held.cert_fingerprint.as_deref(), Some("AB:CD"));
    assert_eq!(
        table.get(&SatelliteHost::new("sandbox")).map(|e| &e.target),
        Some(&SatelliteTarget::Wss {
            url: "wss://sandbox:8787".to_owned(),
        })
    );
}

#[test]
fn diff_names_added_removed_and_changed_links_only() {
    let before = HubTable::from_registry(&[
        entry("kept", "quic://kept:1", true),
        entry("moved", "quic://old:1", true),
        entry("parked", "quic://parked:1", true),
        entry("gone", "quic://gone:1", true),
    ])
    .unwrap();
    let mut repinned = entry("repinned", "quic://pin:1", true);
    let pinned_before = HubTable::from_registry(std::slice::from_ref(&repinned)).unwrap();
    repinned.cert_fingerprint = Some("AB:CD".to_owned());
    let after = HubTable::from_registry(&[
        entry("kept", "quic://kept:1", true),
        entry("moved", "quic://new:1", true),
        entry("parked", "quic://parked:1", false),
        entry("fresh", "ssh://fresh", true),
    ])
    .unwrap();
    let host = |name: &str| SatelliteHost::new(name);
    assert_eq!(
        before.diff(&after),
        TableDiff {
            added: vec![host("fresh")],
            removed: vec![host("gone"), host("parked")],
            changed: vec![host("moved")],
        }
    );
    assert!(after.diff(&after).is_empty());
    let pinned_after = HubTable::from_registry(&[repinned]).unwrap();
    assert_eq!(
        pinned_before.diff(&pinned_after).changed,
        [host("repinned")]
    );
    assert_eq!(
        HubTable::default().diff(&after).added,
        [host("fresh"), host("kept"), host("moved")]
    );
}

#[test]
fn duplicate_names_rejected_even_when_one_is_disabled() {
    for first_enabled in [true, false] {
        let err = HubTable::from_registry(&[
            entry("devbox", "quic://a:1", first_enabled),
            entry("devbox", "quic://b:2", true),
        ])
        .unwrap_err();
        assert_eq!(
            err,
            HubTableError::DuplicateName {
                name: "devbox".to_owned(),
            }
        );
    }
}

#[test]
fn half_a_client_identity_is_refused_by_name() {
    let mut half = entry("devbox", "quic://devbox:8788", true);
    half.client_cert = Some(PathBuf::from("/state/devbox.pem"));
    let err = HubTable::from_registry(&[half]).unwrap_err();
    assert!(
        matches!(&err, HubTableError::ClientIdentity { name, .. } if name == "devbox"),
        "{err:?}"
    );
}

#[test]
fn malformed_enabled_entry_fails_with_name_and_endpoint() {
    let err = HubTable::from_registry(&[entry("devbox", "gopher://devbox", true)]).unwrap_err();
    match err {
        HubTableError::MalformedEndpoint {
            name,
            endpoint,
            reason,
        } => {
            assert_eq!(name, "devbox");
            assert_eq!(endpoint, "gopher://devbox");
            assert!(reason.contains("unsupported scheme"), "{reason}");
        }
        other @ (HubTableError::DuplicateName { .. } | HubTableError::ClientIdentity { .. }) => {
            panic!("expected MalformedEndpoint, got {other:?}")
        }
    }
}

// --- hub gate ------------------------------------------------------

#[test]
fn only_hub_mode_validates_the_registry() {
    let garbage = [
        entry("devbox", "not a uri", true),
        entry("devbox", "also broken", true),
    ];
    assert_eq!(resolve_hub_table(false, &garbage), Ok(None));
    let table = resolve_hub_table(true, &[entry("devbox", "ssh://devbox", true)]);
    assert_eq!(table.unwrap().unwrap().len(), 1);
    assert!(resolve_hub_table(true, &[entry("devbox", "nope", true)]).is_err());
    assert!(resolve_hub_table(true, &[]).unwrap().unwrap().is_empty());
}
