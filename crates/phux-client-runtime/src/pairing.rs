//! Canonical connect-link parsing, shared by the CLI and native hosts.
//!
//! Parsing preserves legacy bearer formats. An enrolling host must separately
//! require the transport, trust pins and credential shapes its platform needs.

use std::collections::HashSet;

/// Maximum imported link size, including any pasted quoting or whitespace.
pub const MAX_CONNECT_LINK_BYTES: usize = 16 * 1024;

/// The connect link emitted by `phux pair` and encoded in its QR.
pub const CONNECT_URI_PREFIX: &str = "https://phux.sh/connect";
/// The pre-`phux.sh` host, retained for saved links.
pub const LEGACY_HOST_CONNECT_URI_PREFIX: &str = "https://phux.phall.io/connect";
/// The historical app scheme, retained for saved links and CLI codes.
pub const LEGACY_CONNECT_URI_PREFIX: &str = "phux://connect";

/// Credentials from a connect link. Treat the entire value as secret.
#[derive(Clone, PartialEq, Eq)]
pub struct ConnectLink {
    /// WebSocket fallback; absent on QUIC-only direct or relay links.
    pub url: Option<String>,
    /// Preferred QUIC endpoint, or the relay endpoint.
    pub quic: Option<String>,
    /// TLS SNI used to route a relay connection.
    pub tls_server_name: Option<String>,
    /// Optional operator-supplied friendly label.
    pub name: Option<String>,
    /// Leaf certificate fingerprint, in its original spelling.
    pub cert_fingerprint: Option<String>,
    /// Canonical `sha256:` certificate authority fingerprint.
    pub ca_fingerprint: Option<String>,
    /// Single-use enrollment ticket. Never log this value.
    pub enrollment_ticket: Option<String>,
    /// Bearer credential. Never log this value.
    pub token: String,
}

impl std::fmt::Debug for ConnectLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Even labels/endpoints are attacker-controlled and could contain a
        // copied token. Do not expose any imported field through diagnostics.
        f.debug_struct("ConnectLink").finish_non_exhaustive()
    }
}

impl ConnectLink {
    /// Registry selection stays compatible with the CLI: WebSocket for a
    /// direct link when supplied, otherwise QUIC. Routed links select QUIC.
    /// Native hosts may prefer QUIC even when a WebSocket fallback is supplied.
    #[must_use]
    pub fn registry_endpoint(&self) -> Option<&str> {
        if self.tls_server_name.is_some() {
            self.quic.as_deref()
        } else {
            self.url.as_deref().or(self.quic.as_deref())
        }
    }
}

/// Parse the exact known link prefixes, tolerating unknown additive query keys.
///
/// Literal `+` stays `+` (this is a URI query, not HTML form encoding).
/// Shell quoting and outer whitespace remain accepted for CLI pastes.
///
/// # Errors
///
/// Refuses oversized links, malformed structure/escapes, controls, duplicate
/// decoded keys, invalid endpoints/SNI/CA, or a missing bearer. Errors never
/// include imported values. Legacy token and leaf-pin shapes are preserved.
pub fn parse_connect_link(link: &str) -> Result<ConnectLink, String> {
    let query = connect_query(link)?;
    let fields = parse_connect_fields(query)?;
    let tls_server_name = parse_sni(fields.sni)?;
    let (url, quic) =
        validate_connect_endpoints(fields.url, fields.quic, tls_server_name.is_some())?;
    let token =
        nonempty(fields.token).ok_or("connect code carries no `token=` — it grants no access")?;
    Ok(ConnectLink {
        url,
        quic,
        tls_server_name,
        name: nonempty(fields.name),
        cert_fingerprint: nonempty(fields.fingerprint),
        ca_fingerprint: parse_authority(fields.authority)?,
        enrollment_ticket: nonempty(fields.enrollment),
        token,
    })
}

fn connect_query(link: &str) -> Result<&str, String> {
    if link.len() > MAX_CONNECT_LINK_BYTES {
        return Err("connect code exceeds 16 KiB".into());
    }
    let trimmed = link.trim().trim_matches(['\'', '"']);
    // CLI pastes historically allow outer newlines, but no controls or raw
    // whitespace may occur inside the URI. Native hosts reject outer controls too.
    if trimmed.chars().any(char::is_control) || trimmed.chars().any(char::is_whitespace) {
        return Err("connect code contains control characters or unescaped whitespace".into());
    }
    if trimmed.contains('#') {
        return Err("connect code must not contain an outer URI fragment".into());
    }
    [
        CONNECT_URI_PREFIX,
        LEGACY_HOST_CONNECT_URI_PREFIX,
        LEGACY_CONNECT_URI_PREFIX,
    ]
    .into_iter()
    .find_map(|prefix| trimmed.strip_prefix(prefix)?.strip_prefix('?'))
    .ok_or_else(|| {
        "a connect code must be the whole https://phux.sh/connect? or phux://connect? link".into()
    })
}

fn nonempty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.is_empty())
}

fn parse_sni(sni: Option<String>) -> Result<Option<String>, String> {
    nonempty(sni)
        .map(|sni| {
            crate::target::validate_tls_server_name(&sni)
                .map_err(|_| "connect code sni must be a DNS relay route".into())
        })
        .transpose()
}

fn parse_authority(authority: Option<String>) -> Result<Option<String>, String> {
    nonempty(authority)
        .map(|ca| {
            phux_config_core::known_authorities::canonical_authority(&ca)
                .ok_or_else(|| "connect code ca must be a sha256: certificate fingerprint".into())
        })
        .transpose()
}

#[derive(Default)]
struct ConnectFields {
    url: Option<String>,
    quic: Option<String>,
    sni: Option<String>,
    name: Option<String>,
    fingerprint: Option<String>,
    authority: Option<String>,
    enrollment: Option<String>,
    token: Option<String>,
}

fn parse_connect_fields(query: &str) -> Result<ConnectFields, String> {
    let mut fields = ConnectFields::default();
    let mut seen = HashSet::new();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, raw) = pair
            .split_once('=')
            .ok_or("connect code field has no value")?;
        let key = percent_decode(key)?;
        let value = percent_decode(raw)?;
        if !seen.insert(key.clone()) {
            return Err("connect code carries duplicate query fields".into());
        }
        set_connect_field(&mut fields, &key, value);
    }
    Ok(fields)
}

fn set_connect_field(fields: &mut ConnectFields, key: &str, value: String) {
    let slot = match key {
        "url" => &mut fields.url,
        "quic" => &mut fields.quic,
        "sni" => &mut fields.sni,
        "name" => &mut fields.name,
        "fp" => &mut fields.fingerprint,
        "ca" => &mut fields.authority,
        "enroll" => &mut fields.enrollment,
        "token" => &mut fields.token,
        _ => return,
    };
    *slot = Some(value);
}

fn validate_connect_endpoints(
    url: Option<String>,
    quic: Option<String>,
    routed: bool,
) -> Result<(Option<String>, Option<String>), String> {
    let url = nonempty(url);
    let quic = nonempty(quic);
    if routed && quic.is_none() {
        return Err(
            "connect code carries `sni=` but no `quic=` — a relay route is dialed over QUIC".into(),
        );
    }
    if !routed && url.is_none() && quic.is_none() {
        return Err(
            "connect code carries no `url=` or `quic=` — it cannot name a server to dial".into(),
        );
    }
    if let Some(url) = &url {
        validate_ws_endpoint(url)?;
    }
    if let Some(endpoint) = &quic {
        validate_quic_endpoint(endpoint)?;
    }
    Ok((url, quic))
}

fn validate_ws_endpoint(url: &str) -> Result<(), String> {
    if !url.starts_with("wss://") && !url.starts_with("ws://") {
        return Err("connect code url must be ws:// or wss://".into());
    }
    Ok(())
}

/// Check a pairing QUIC endpoint without dialing or exposing it in errors.
///
/// # Errors
///
/// Requires `quic://HOST:PORT`, with a nonzero numeric port and no userinfo,
/// path, query or fragment. IPv6 hosts must be bracketed.
pub fn validate_quic_endpoint(endpoint: &str) -> Result<(), String> {
    let authority = endpoint
        .strip_prefix("quic://")
        .ok_or("connect code quic endpoint must start with quic://")?;
    let (host, port) = authority
        .rsplit_once(':')
        .ok_or("connect code quic endpoint needs HOST:PORT")?;
    if !valid_quic_host(host) || authority.contains(['/', '?', '#', '@']) {
        return Err("connect code quic endpoint needs a valid HOST:PORT".into());
    }
    if !port.parse::<u16>().is_ok_and(|port| port != 0) {
        return Err("connect code quic endpoint needs a non-zero numeric port".into());
    }
    Ok(())
}

fn valid_quic_host(host: &str) -> bool {
    if host.chars().any(char::is_whitespace) || host.chars().any(char::is_control) {
        return false;
    }
    if let Some(inner) = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
    {
        return inner.parse::<std::net::Ipv6Addr>().is_ok();
    }
    !host.is_empty() && !host.contains([':', '[', ']'])
}

fn percent_decode(value: &str) -> Result<String, String> {
    let mut bytes = value.bytes();
    let mut out = Vec::with_capacity(value.len());
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let hi = bytes
                .next()
                .and_then(hex_digit)
                .ok_or("invalid connect code percent-escape")?;
            let lo = bytes
                .next()
                .and_then(hex_digit)
                .ok_or("invalid connect code percent-escape")?;
            out.push((hi << 4) | lo);
        } else {
            out.push(byte);
        }
    }
    let decoded = String::from_utf8(out).map_err(|_| "connect code field is not UTF-8")?;
    if decoded.chars().any(char::is_control) {
        return Err("connect code field contains a control character".into());
    }
    Ok(decoded)
}

const fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIRECT: &str =
        "https://phux.sh/connect?url=wss://mini:8787&quic=quic://mini:8788&token=legacy-token";

    #[test]
    fn pairing_preserves_cli_endpoint_selection_and_legacy_credentials() {
        let direct = parse_connect_link(DIRECT).unwrap();
        assert_eq!(direct.token, "legacy-token");
        assert_eq!(direct.registry_endpoint(), Some("wss://mini:8787"));
        assert_eq!(direct.quic.as_deref(), Some("quic://mini:8788"));
        for prefix in [LEGACY_CONNECT_URI_PREFIX, LEGACY_HOST_CONNECT_URI_PREFIX] {
            assert_eq!(
                parse_connect_link(&DIRECT.replace(CONNECT_URI_PREFIX, prefix)).unwrap(),
                direct
            );
        }
        assert_eq!(
            parse_connect_link(&format!("  '{DIRECT}'\n")).unwrap(),
            direct
        );
        let relay =
            parse_connect_link("phux://connect?quic=quic://relay:4433&sni=mini-route&token=t")
                .unwrap();
        assert_eq!(relay.registry_endpoint(), Some("quic://relay:4433"));
        assert_eq!(relay.tls_server_name.as_deref(), Some("mini-route"));
    }

    #[test]
    fn pairing_accepts_direct_quic_without_inventing_a_websocket_fallback() {
        let direct =
            parse_connect_link("https://phux.sh/connect?quic=quic://mini:8788&token=t").unwrap();
        assert_eq!(direct.url, None);
        assert_eq!(direct.tls_server_name, None);
        assert_eq!(direct.registry_endpoint(), Some("quic://mini:8788"));
        for query in [
            "token=t",
            "url=&quic=&token=t",
            "quic=quic://mini:0&token=t",
            "quic=quic://user@mini:8788&token=t",
            "quic=ws://mini:8788&token=t",
            "url=wss://mini:8787&sni=route&token=t",
        ] {
            assert!(
                parse_connect_link(&format!("https://phux.sh/connect?{query}")).is_err(),
                "{query}"
            );
        }
    }

    #[test]
    fn pairing_decodes_once_and_preserves_literal_plus() {
        let parsed = parse_connect_link(&format!(
            "{DIRECT}&name=mini+box%20%F0%9F%A6%80&future=%2520"
        ))
        .unwrap();
        assert_eq!(parsed.name.as_deref(), Some("mini+box 🦀"));
        assert_eq!(
            parse_connect_link(&DIRECT.replace("token=legacy-token", "t%6fken=a%2Bb+c"))
                .unwrap()
                .token,
            "a+b+c"
        );
    }

    #[test]
    fn pairing_refuses_duplicates_even_with_escaped_keys() {
        for extra in [
            "token=other",
            "t%6fken=other",
            "url=wss://other:1",
            "name=a&name=b",
            "enroll=a&enroll=b",
            "future=a&future=b",
        ] {
            assert!(parse_connect_link(&format!("{DIRECT}&{extra}")).is_err());
        }
    }

    #[test]
    fn pairing_checks_the_whole_outer_uri() {
        for prefix in [
            "https://phux.sh/connect/",
            "https://phux.sh/connect.evil",
            "https://phux.sh@evil/connect",
            "https://phux.sh:444/connect",
            "https://phux.sh/other/../connect",
            "phux://connect/",
            "phux-rune://connect",
        ] {
            assert!(parse_connect_link(&DIRECT.replace(CONNECT_URI_PREFIX, prefix)).is_err());
        }
        for suffix in ["#fragment", "#", "\0", "&name=raw space", "&token=\nsecret"] {
            assert!(parse_connect_link(&format!("{DIRECT}{suffix}")).is_err());
        }
    }

    #[test]
    fn pairing_refuses_malformed_escapes_controls_and_large_imports() {
        for value in [
            "%",
            "%0",
            "%GG",
            "%FF",
            "%00",
            "%0A",
            "%7f",
            "%C2%85",
            "raw\0",
            "raw\tvalue",
        ] {
            assert!(parse_connect_link(&format!("{DIRECT}&name={value}")).is_err());
            assert!(parse_connect_link(&format!("{DIRECT}&{value}=unknown")).is_err());
        }
        let at_limit = format!(
            "{DIRECT}&name={}",
            "a".repeat(MAX_CONNECT_LINK_BYTES - DIRECT.len() - 6)
        );
        assert!(parse_connect_link(&at_limit).is_ok());
        assert!(parse_connect_link(&(at_limit + "a")).is_err());
    }

    #[test]
    fn pairing_errors_and_debug_never_echo_imported_secrets() {
        let secret = "unique-private-bearer";
        for field in [
            secret.to_owned(),
            format!("sni={secret}%20bad"),
            format!("quic={secret}"),
            format!("url={secret}"),
            format!("ca={secret}"),
            format!("name={secret}%GG"),
            format!("name={secret}%FF"),
        ] {
            let error =
                parse_connect_link(&format!("phux://connect?{field}&token={secret}")).unwrap_err();
            assert!(!error.contains(secret));
        }
        let debug = format!(
            "{:?}",
            parse_connect_link(&format!("{DIRECT}&name={secret}&enroll={secret}")).unwrap()
        );
        assert!(!debug.contains(secret));
        assert!(!debug.contains("legacy-token"));
    }
}
