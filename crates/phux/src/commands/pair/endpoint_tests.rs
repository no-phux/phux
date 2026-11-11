//! Endpoint selection and fail-closed relay routing for pasted connect codes.

use super::parse_connect_link;

/// Old links keep their supplied WebSocket URL; routed links select QUIC.
#[test]
fn registry_endpoint_is_the_url_unless_the_link_is_routed() {
    let direct = parse_connect_link(
        "https://phux.sh/connect?url=wss://mini:8787&quic=quic://mini:8788&token=tok",
    )
    .expect("parse");
    assert_eq!(direct.registry_endpoint(), Some("wss://mini:8787"));
    let both = parse_connect_link(
        "https://phux.sh/connect?url=wss://mini:8787&quic=quic://relay:4433&sni=r&token=tok",
    )
    .expect("parse");
    assert_eq!(both.registry_endpoint(), Some("quic://relay:4433"));
    assert_eq!(both.tls_server_name.as_deref(), Some("r"));
    // Empty SNI is absent, not a relay route. Direct QUIC needs no WS leg.
    let quic = parse_connect_link("https://phux.sh/connect?quic=quic://mini:8788&sni=&token=t")
        .expect("direct QUIC-only code");
    assert_eq!(quic.registry_endpoint(), Some("quic://mini:8788"));
    assert_eq!(quic.tls_server_name, None);
}

/// `sni` needs QUIC, valid DNS routing and an unambiguous single field.
#[test]
fn relay_links_that_cannot_dial_are_refused() {
    let no_quic = parse_connect_link("https://phux.sh/connect?url=wss://m:1&sni=r&token=t")
        .expect_err("sni needs quic");
    assert!(no_quic.contains("quic="), "{no_quic}");
    for bad in ["-r", "127.0.0.1", "a%20b", "a/b"] {
        assert!(
            parse_connect_link(&format!(
                "https://phux.sh/connect?quic=quic://relay:4433&sni={bad}&token=t"
            ))
            .is_err(),
            "accepted sni={bad}"
        );
    }
    assert!(
        parse_connect_link("https://phux.sh/connect?quic=quic://relay:4433&sni=a&sni=b&token=t")
            .is_err()
    );
}
