//! The link's `name`: what a phone and the connect page list the host as.

/// The name a link carries (#2399): `--name` when given, else the first label
/// of the `MagicDNS` name the link dials by, else this machine's short host
/// name. A link without one lists the host by a placeholder on the phone.
pub(super) fn link_name(explicit: Option<&str>, magic_dns_name: Option<&str>) -> String {
    let tailnet = magic_dns_name.and_then(|name| name.split('.').next());
    explicit
        .or(tailnet)
        .filter(|name| !name.is_empty())
        .map_or_else(phux_config::instance::local_host_name, str::to_owned)
}

/// Percent-encode everything outside RFC 3986 `unreserved` — conservative on
/// purpose, since the value lands inside a URI query a phone must parse.
pub(super) fn percent_encode(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                out.push('%');
                out.push(HEX[usize::from(byte >> 4)] as char);
                out.push(HEX[usize::from(byte & 0x0F)] as char);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::link_name;

    #[test]
    fn the_link_names_the_machine() {
        assert_eq!(
            link_name(Some("studio mini"), Some("fr-host.tail9a8b.ts.net")),
            "studio mini"
        );
        assert_eq!(link_name(None, Some("fr-host.tail9a8b.ts.net")), "fr-host");
        let host = phux_config::instance::local_host_name();
        assert_eq!(link_name(None, None), host);
        assert_eq!(
            link_name(Some(""), None),
            host,
            "an empty --name is no name"
        );
    }
}
