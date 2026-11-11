//! Control selectors (`page`, `window`, `display`, each alone or
//! `:<host>`): the only selectors besides `resource:` that reach document
//! input (`docs/resource-kinds.md#authority`).

use super::{grant, host};
use phux_protocol::scope::{ScopeError, ScopeGrammarError, Selector};
use phux_protocol::wire::frame::ControlClass;

#[test]
fn control_selectors_encode_parse_and_display_canonically() {
    let mut satellite = vec![0x06, 0x02, 0x01, 0x00, 0x0c];
    satellite.extend_from_slice(b"powerhouse-1");
    for (text, selector, bytes) in [
        (
            "page",
            Selector::Control(ControlClass::Page, None),
            vec![0x06, 0x01, 0x00],
        ),
        (
            "display",
            Selector::Control(ControlClass::Display, None),
            vec![0x06, 0x03, 0x00],
        ),
        (
            "window:powerhouse-1",
            Selector::Control(ControlClass::Window, Some(host("powerhouse-1"))),
            satellite,
        ),
    ] {
        assert_eq!(Selector::parse(text).unwrap(), selector, "{text}");
        assert_eq!(selector.to_string(), text);
        assert_eq!(selector.to_bytes(), bytes, "{text}");
        assert_eq!(Selector::decode(&bytes).unwrap(), selector, "{text}");
        assert_eq!(selector.kind_name(), "control");
        assert!(
            !selector.names_process_id(),
            "a control selector may persist"
        );
    }
    assert_eq!(
        grant("input@display").selector,
        Selector::Control(ControlClass::Display, None)
    );
}

#[test]
fn control_selectors_refuse_unknown_classes_and_bad_hosts() {
    assert_eq!(
        Selector::parse("Page"),
        Err(ScopeGrammarError::UnknownSelector)
    );
    assert_eq!(
        Selector::parse("screen"),
        Err(ScopeGrammarError::UnknownSelector)
    );
    assert_eq!(
        Selector::parse("page:"),
        Err(ScopeGrammarError::InvalidHost)
    );
    assert_eq!(
        Selector::decode(&[0x06, 0x09, 0x00]),
        Err(ScopeError::UnknownSubtype)
    );
    assert_eq!(
        Selector::decode(&[0x06, 0x01, 0x02]),
        Err(ScopeError::UnknownSubtype)
    );
    assert_eq!(Selector::decode(&[0x06, 0x01]), Err(ScopeError::Truncated));
    assert_eq!(
        Selector::decode(&[0x06, 0x01, 0x00, 0x00]),
        Err(ScopeError::NonMinimalSelector)
    );
}

#[test]
fn a_control_selector_overlaps_its_host_and_no_other_class() {
    let page = Selector::parse("page").unwrap();
    let window_sat = Selector::parse("window:sat").unwrap();
    assert!(page.may_overlap(&Selector::HostLocal));
    assert!(page.may_overlap(&Selector::Global));
    assert!(!page.may_overlap(&Selector::parse("display").unwrap()));
    assert!(!page.may_overlap(&window_sat));
    assert!(window_sat.may_overlap(&Selector::parse("host:sat").unwrap()));
    assert!(!window_sat.may_overlap(&Selector::parse("host:other").unwrap()));
}
