//! The native host's configuration, trust and lifecycle.

use super::*;
use phux_client_runtime::Transport;
use phux_protocol::wire::frame::FrameKind;

#[test]
fn ime_reconversion_replaces_only_the_focused_field_in_both_forms() {
    for (phone, size) in [(false, (1100.0, 720.0)), (true, (393.0, 852.0))] {
        let (client, mut rt) = crate::grid::budget::input_fixture();
        rt.state.phone = phone.then(crate::desk::phone::Phone::default);
        rt.set_viewport(size, 2.0);
        rt.update(|state, tx| {
            state.palette_open = true;
            state.palette_query = rune::TextEdit::new("日本語 input");
            state.palette_query.focused = true;
            tx.instant();
        });
        rt.frame(0.0);
        let (x, y) = rt.find("palette-query").expect("palette field");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.0);
        let _ = client.take_outbound();
        rt.ime(rune::Ime::Select(0..9));
        rt.frame(0.0);
        assert_eq!(rt.state.palette_query.selected(), "日本語");
        let clauses = vec![rune::Clause::new(0..6), rune::Clause::new(6..12).target()];
        rt.ime(rune::Ime::Preedit(
            "にほんご".into(),
            Some((6, 12)),
            clauses.clone(),
        ));
        rt.frame(0.0);
        assert_eq!(rt.state.palette_query.text, "日本語 input");
        assert_eq!(rt.state.palette_query.preedit_clauses(), clauses);
        rt.ime(rune::Ime::Commit("にほんご".into()));
        rt.frame(0.0);
        assert_eq!(rt.state.palette_query.text, "にほんご input");
        assert!(!rt.state.palette_query.is_composing());
        for bytes in client.take_outbound() {
            let frame = FrameKind::decode(&bytes).expect("outbound frame").0;
            assert!(
                matches!(
                    frame,
                    FrameKind::InputFocus { .. } | FrameKind::ResizeTerminal { .. }
                ),
                "field reconversion must not reach the terminal: {frame:?}"
            );
        }
    }
}

#[test]
fn terminal_rejects_reconversion_of_text_it_does_not_own() {
    let (client, mut rt) = crate::grid::budget::input_fixture();
    rt.frame(0.0);
    let _ = client.take_outbound();
    assert!(!rt.state.ime(&rune::Ime::Select(0..9)));
    assert!(rt.state.preedit.is_none());
    assert!(client.take_outbound().is_empty());
}

#[test]
fn inactivity_finishes_a_runtime_field_composition_once_without_terminal_input() {
    let (client, mut rt) = crate::grid::budget::input_fixture();
    rt.state.phone = Some(crate::desk::phone::Phone::default());
    rt.set_viewport((393.0, 852.0), 2.0);
    rt.update(|state, tx| {
        state.palette_open = true;
        state.palette_query = rune::TextEdit::new("");
        state.palette_query.focused = true;
        tx.instant();
    });
    rt.frame(0.0);
    let (x, y) = rt.find("palette-query").expect("palette field");
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    rt.frame(0.0);
    let _ = client.take_outbound();
    rt.ime(rune::Ime::Preedit("日本".into(), Some((0, 6)), Vec::new()));
    rt.frame(0.0);
    assert_eq!(rt.state.palette_query.text, "");
    assert!(rt.state.palette_query.preedit().is_some());
    set_active(&mut rt, false);
    rt.frame(0.0);
    assert_eq!(rt.state.palette_query.text, "日本");
    assert!(rt.state.palette_query.preedit().is_none());
    set_active(&mut rt, true);
    rt.frame(0.0);
    assert_eq!(rt.state.palette_query.text, "日本");
    for bytes in client.take_outbound() {
        let frame = FrameKind::decode(&bytes).expect("outbound frame").0;
        assert!(
            matches!(
                frame,
                FrameKind::InputFocus { .. } | FrameKind::ResizeTerminal { .. }
            ),
            "lifecycle may report focus/fit, but never sends field text: {frame:?}"
        );
    }
}

#[test]
fn relay_configuration_rejects_invalid_sni_without_dropping_it() {
    let mut configuration = config("quic://localhost:8788");
    for route in ["", "127.0.0.1", "secret/bad", "-bad", "secret\0"] {
        configuration.tls_server_name = Some(route.into());
        let error = configuration.target().unwrap_err();
        assert!(!error.contains("secret"));
    }
    configuration.tls_server_name = Some("mini-route".into());
    assert!(configuration.target().is_ok());
    configuration.endpoint = "wss://localhost:8787".into();
    assert!(configuration.target().is_err());
}

fn config(endpoint: &str) -> Configuration {
    Configuration {
        endpoint: endpoint.into(),
        name: None,
        tls_server_name: None,
        certificate_pin: None,
        token: None,
        session: None,
        authority_ca: None,
        chain_pem: None,
        key_id: None,
    }
}

#[test]
fn remote_enrollment_and_connection_have_distinct_requirements() {
    let mut configuration = config("quic://example.com:8788");
    configuration.certificate_pin = Some("ab".repeat(32));
    configuration.token = Some("cd".repeat(32));
    assert!(configuration.target().is_ok());
    assert!(configuration.enrollment_target().is_err());
    assert!(configuration.validate_connection().is_err());
    configuration.authority_ca = Some(format!("sha256:{}", "ef".repeat(32)));
    let target = configuration.enrollment_target().unwrap();
    assert_eq!(target.authority.ca, configuration.authority_ca);
    configuration.chain_pem = Some("public certificate".into());
    assert!(configuration.validate_connection().is_err());
    configuration.key_id = Some("device-key".into());
    assert!(configuration.validate_connection().is_ok());
    assert!(state_with_identity(configuration, None).is_err());
}

#[test]
fn mobile_rejects_malformed_credentials_and_wss_enrollment() {
    let mut configuration = config("quic://localhost:8788");
    for pin in ["no", "sha256:", &"x".repeat(64)] {
        configuration.authority_ca = Some(pin.into());
        assert!(configuration.target().is_err());
    }
    configuration.authority_ca = Some(format!("sha256:{}", "ab:".repeat(31) + "ab"));
    assert!(configuration.enrollment_target().is_ok());
    for token in ["secret", "a", "00\n"] {
        configuration.token = Some(token.into());
        assert!(configuration.target().is_err());
    }
    configuration.token = None;
    configuration.endpoint = "wss://localhost:8788".into();
    assert!(configuration.target().is_ok());
    assert!(configuration.enrollment_target().is_err());
}

#[test]
fn configuration_round_trips_public_identity_fields_and_rejects_tickets() {
    let configuration: Configuration = serde_json::from_value(json!({
        "endpoint": "quic://localhost:8788",
        "authority_ca": format!("sha256:{}", "ab".repeat(32)),
        "chain_pem": "public chain",
        "key_id": "persistent-key-tag"
    }))
    .unwrap();
    assert_eq!(configuration.chain_pem.as_deref(), Some("public chain"));
    assert_eq!(configuration.key_id.as_deref(), Some("persistent-key-tag"));
    assert!(configuration.enrollment_target().is_ok());
    assert!(
        serde_json::from_value::<Configuration>(json!({
            "endpoint": "quic://localhost:8788", "ticket": "not persisted"
        }))
        .is_err()
    );
    let unpaired = config("quic://localhost:8788");
    assert!(unpaired.validate_connection().is_ok());
    assert!(matches!(
        unpaired.target_with_identity(None).unwrap().client_identity,
        TlsClientIdentity::None
    ));
}

#[test]
fn mobile_never_infers_remote_trust_or_a_local_cli() {
    for endpoint in [
        "ws://example.com/",
        "wss://example.com/",
        "quic://example.com:8788",
        "file:///tmp/s",
        "quic://localhost/path",
        "wss://secret@example.com/",
    ] {
        assert!(config(endpoint).target().is_err(), "{endpoint}");
    }
    for endpoint in [
        "ws://127.0.0.1:9999/",
        "quic://localhost:8788",
        "quic://[::1]:8788",
    ] {
        assert!(config(endpoint).target().is_ok());
    }
    assert_eq!(
        config("quic://[::1]:8788").target().unwrap().transport,
        Transport::Quic("[::1]:8788".into())
    );
    let resolved = crate::launch::Resolved::embedded(None);
    assert!(!resolved.can_ensure());
    assert!(!resolved.missing_cli());
    assert!(resolved.ensured().phux.is_none());
}

#[test]
fn a_paired_name_is_what_the_window_says_and_never_what_it_dials() {
    let named = |name: serde_json::Value| -> Configuration {
        serde_json::from_value(json!({"endpoint": "quic://localhost:8788", "name": name}))
            .expect("a configuration")
    };
    let target = named(json!("  Phil's Mac mini ")).target().unwrap();
    assert_eq!(target.name, "Phil's Mac mini");
    assert_eq!(target.transport, Transport::Quic("localhost:8788".into()));
    let paired = state(named(json!("mini"))).unwrap();
    assert_eq!(paired.machines.label(), "mini");
    let native = paired.native.as_ref().unwrap();
    assert_eq!(
        native.target.transport,
        Transport::Quic("localhost:8788".into())
    );
    // Nothing fit to show leaves the endpoint to speak for itself.
    for unnamed in [json!(null), json!(""), json!("   "), json!("mini\nrm -rf")] {
        assert_eq!(named(unnamed).target().unwrap().name, "localhost:8788");
    }
    // Settings saved before pairing carried a name still open.
    let saved: Configuration =
        serde_json::from_value(json!({"endpoint": "quic://localhost:8788"})).unwrap();
    assert_eq!(saved.name, None);
    assert_eq!(state(saved).unwrap().machines.label(), "localhost:8788");
}

#[test]
fn an_absence_is_measured_on_the_wall_clock_from_its_first_edge() {
    let at = |secs| SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
    let mut state = with_target(config("quic://localhost:8788").target().unwrap(), None);
    let native = state.native.as_mut().unwrap();
    // Becoming active with no absence behind it measures nothing.
    assert_eq!(native.set_active(true, at(100)), None);
    // Resigning and then backgrounding is one absence.
    assert_eq!(native.set_active(false, at(100)), None);
    assert_eq!(native.set_active(false, at(130)), None);
    assert!(!native.active);
    assert_eq!(
        native.set_active(true, at(145)),
        Some(Duration::from_secs(45))
    );
    assert!(native.active && native.away_since.is_none());
    // A clock set back while away is no time at all, not a panic.
    assert_eq!(native.set_active(false, at(200)), None);
    assert_eq!(native.set_active(true, at(150)), Some(Duration::ZERO));
}
