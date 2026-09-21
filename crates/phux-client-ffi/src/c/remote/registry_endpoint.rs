//! Syntax-only endpoint validation. Never resolves DNS or reads credentials.

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Direct,
    Ssh,
}

#[derive(PartialEq, Eq)]
enum Port {
    Absent,
    Explicit,
}

pub(super) fn parse(endpoint: &str) -> Option<Kind> {
    let (scheme, authority) = parts(endpoint)?;
    let kind = scheme_kind(scheme)?;
    let host_port = without_userinfo(authority, kind)?;
    let port = authority_port(host_port)?;
    if scheme == "quic" && port == Port::Absent {
        return None;
    }
    Some(kind)
}

fn parts(endpoint: &str) -> Option<(&str, &str)> {
    let endpoint = endpoint.trim();
    if endpoint
        .chars()
        .any(|c| c.is_control() || c.is_whitespace())
    {
        return None;
    }
    if endpoint.contains(['?', '#', '\\']) {
        return None;
    }
    let (scheme, rest) = endpoint.split_once("://")?;
    let path = rest.split_once('/');
    if path.is_some() && !matches!(scheme, "ws" | "wss") {
        return None;
    }
    let authority = path.map_or(rest, |(authority, _)| authority);
    Some((scheme, authority))
}

fn scheme_kind(scheme: &str) -> Option<Kind> {
    match scheme {
        "quic" | "ws" | "wss" => Some(Kind::Direct),
        "ssh" => Some(Kind::Ssh),
        _ => None,
    }
}

fn without_userinfo(authority: &str, kind: Kind) -> Option<&str> {
    let Some((user, host)) = authority.rsplit_once('@') else {
        return Some(authority);
    };
    // Direct transports authenticate via token/pin, never URI userinfo. Refuse
    // multiple @ and percent-encoded userinfo rather than displaying secrets.
    if kind != Kind::Ssh {
        return None;
    }
    if user.is_empty() || user.contains([':', '@', '%']) {
        return None;
    }
    Some(host)
}

fn authority_port(authority: &str) -> Option<Port> {
    if let Some(inner) = authority.strip_prefix('[') {
        let (host, suffix) = inner.split_once(']')?;
        host.parse::<std::net::Ipv6Addr>().ok()?;
        return optional_port(suffix);
    }
    let (host, suffix) = authority
        .split_once(':')
        .map_or((authority, ""), |(h, _)| (h, &authority[h.len()..]));
    if !valid_host(host) {
        return None;
    }
    optional_port(suffix)
}

fn valid_host(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    host.strip_suffix('.')
        .unwrap_or(host)
        .split('.')
        .all(valid_label)
}

fn valid_label(label: &str) -> bool {
    if label.is_empty() || label.len() > 63 || label.starts_with('-') || label.ends_with('-') {
        return false;
    }
    label
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
}

fn optional_port(suffix: &str) -> Option<Port> {
    if suffix.is_empty() {
        return Some(Port::Absent);
    }
    let digits = suffix.strip_prefix(':')?;
    if !digits.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u16>().ok().filter(|port| *port > 0)?;
    Some(Port::Explicit)
}
