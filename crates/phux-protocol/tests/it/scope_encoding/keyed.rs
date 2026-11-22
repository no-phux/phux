//! Keyed session and resource selectors.

use super::{grant, host};
use phux_protocol::scope::{ScopeError, ScopeGrammarError, ScopeGrant, Selector};

/// The 26-character text form of a key whose 16 bytes are all `byte`.
fn key_text(byte: u8) -> String {
    phux_protocol::ids::SessionKey::new([byte; 16])
        .unwrap()
        .to_string()
}

#[test]
fn keyed_selectors_encode_parse_and_refuse_noncanonical_keys() {
    use phux_protocol::ids::{ResourceKey, SessionKey};

    let session = SessionKey::new([0x11; 16]).unwrap();
    let resource = ResourceKey::new([0x22; 16]).unwrap();
    let mut session_bytes = vec![0x04];
    session_bytes.extend_from_slice(&[0x11; 16]);
    let mut resource_bytes = vec![0x05];
    resource_bytes.extend_from_slice(&[0x22; 16]);
    for (selector, bytes) in [
        (Selector::Session(session), session_bytes),
        (Selector::Resource(resource), resource_bytes),
    ] {
        assert_eq!(selector.to_bytes(), bytes, "{selector}");
        assert_eq!(Selector::decode(&bytes).unwrap(), selector, "{selector}");
        // The registry spelling round-trips through Display.
        assert_eq!(Selector::parse(&selector.to_string()).unwrap(), selector);
        assert!(!selector.names_process_id(), "{selector}");
    }
    assert_eq!(
        grant(&format!("observe,input@session:{}", key_text(0x11))).selector,
        Selector::Session(session)
    );

    // An all-zero key is no record's, and a short key is truncated.
    let mut zero = vec![0x04];
    zero.extend_from_slice(&[0; 16]);
    assert_eq!(Selector::decode(&zero), Err(ScopeError::InvalidKey));
    assert_eq!(
        Selector::decode(&[0x05, 0x01, 0x02]),
        Err(ScopeError::Truncated)
    );
    let mut long = vec![0x05];
    long.extend_from_slice(&[0x22; 17]);
    assert_eq!(Selector::decode(&long), Err(ScopeError::NonMinimalSelector));

    // One spelling per key: the full lowercase text form only.
    let text = key_text(0x11);
    for bad in [
        text.to_uppercase(),
        text[..6].to_owned(),
        format!("^{}", &text[..6]),
        String::new(),
        "a".repeat(26),
    ] {
        assert_eq!(
            ScopeGrant::parse(&format!("observe@session:{bad}")),
            Err(ScopeGrammarError::InvalidKey),
            "{bad}"
        );
        assert_eq!(
            ScopeGrant::parse(&format!("observe@resource:{bad}")),
            Err(ScopeGrammarError::InvalidKey),
            "{bad}"
        );
    }

    // The id-named selectors are the ones a persisted record cannot hold.
    for selector in ["group:7", "terminal:9", "terminal:devbox/9"] {
        assert!(
            grant(&format!("observe@{selector}"))
                .selector
                .names_process_id()
        );
    }
    for selector in ["global", "host", "host:devbox"] {
        assert!(
            !grant(&format!("observe@{selector}"))
                .selector
                .names_process_id()
        );
    }
}

#[test]
fn keyed_selectors_overlap_every_local_selector_of_the_other_spelling() {
    use phux_protocol::ids::{ResourceKey, SessionKey};

    let session = |byte| Selector::Session(SessionKey::new([byte; 16]).unwrap());
    let resource = |byte| Selector::Resource(ResourceKey::new([byte; 16]).unwrap());
    // A key and a wire id may name the same session or Terminal.
    for (a, b) in [
        (session(1), Selector::Group(7)),
        (session(1), Selector::TerminalLocal(9)),
        (session(1), resource(2)),
        (resource(2), Selector::TerminalLocal(9)),
        (resource(2), Selector::Group(7)),
        (Selector::HostLocal, session(1)),
        (Selector::HostLocal, resource(2)),
    ] {
        assert!(a.may_overlap(&b), "{a} / {b}");
        assert!(b.may_overlap(&a), "{b} / {a}");
    }
    // Two keys name two records; a satellite never holds a local record.
    for (a, b) in [
        (session(1), session(3)),
        (resource(2), resource(4)),
        (session(1), Selector::HostSatellite(host("devbox"))),
        (resource(2), Selector::TerminalSatellite(host("devbox"), 9)),
    ] {
        assert!(!a.may_overlap(&b), "{a} / {b}");
        assert!(!b.may_overlap(&a), "{b} / {a}");
    }
}
