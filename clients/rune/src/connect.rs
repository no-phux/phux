//! Connect links the system opens this app with: `phux://connect?...`, or
//! the `https://phux.sh/connect?...` link `phux pair` prints. The link comes
//! from anyone who can run `open` or `xdg-open`, so it is parsed with the
//! shared parser and offered for the person to accept before anything
//! runs. Accepted, it goes to `phux host add NAME --code -` on stdin, which
//! owns enrollment and the registry: its token never reaches an argument
//! list.

use phux_client_runtime::pairing::{ConnectLink, parse_connect_link};
use phux_client_runtime::target::endpoint_host;

use crate::machines::{Add, Secret};

/// Whether a launch argument is a link for this app to pair from rather
/// than a flag. Anything else with a scheme is refused by [`request`].
pub fn is_link(arg: &str) -> bool {
    !arg.starts_with('-') && arg.contains("://")
}

/// A link waiting for the person to accept it: the add it asks for, and
/// where it dials, to show before anything is contacted or registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    pub request: Add,
    pub peer: String,
}

/// What a link offers, or why it can't be used. Errors never repeat the
/// link: it carries a bearer token.
pub fn request(url: &str) -> Result<Offer, String> {
    let link = parse_connect_link(url)
        .map_err(|why| format!("This pairing link can't be used: {why}."))?;
    let name = name(&link).ok_or("This pairing link names no machine to register.")?;
    let endpoint = link.registry_endpoint().unwrap_or_default();
    let peer = link.tls_server_name.as_ref().map_or_else(
        || endpoint.to_owned(),
        |route| format!("{route} via {endpoint}"),
    );
    let request = Add {
        target: name,
        name: None,
        tailnet: false,
        code: Some(Secret(url.trim().to_owned())),
    };
    Ok(Offer { request, peer })
}

/// What the machine is called here: the name `phux pair --name` gave it,
/// else its relay route, else the host it is dialed at; whichever first
/// makes a registry name.
fn name(link: &ConnectLink) -> Option<String> {
    let host = link.registry_endpoint().and_then(endpoint_host);
    [link.name.clone(), link.tls_server_name.clone(), host]
        .into_iter()
        .flatten()
        .find_map(|candidate| plain(&candidate))
}

/// The longest name kept, as the machines sheet keeps.
const MAX_NAME: usize = 64;

/// `candidate` as a name `phux host add` takes and a person can type after
/// `phux attach`: letters, digits, `-`, `_` and `.`, every other character
/// a `-`, never starting with a selector sigil.
fn plain(candidate: &str) -> Option<String> {
    let mapped: String = candidate
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let name = mapped.trim_matches(['-', '.']);
    (!name.is_empty() && name.chars().count() <= MAX_NAME).then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FP: &str = "abababababababababababababababababababababababababababababababab";
    const TOKEN: &str = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";

    fn target(url: &str) -> Result<String, String> {
        request(url).map(|offer| offer.request.target)
    }

    #[test]
    fn every_spelling_of_the_link_pairs() {
        let query = format!(
            "url=wss://mini.tail.ts.net:8787&quic=quic://mini.tail.ts.net:8788&fp={FP}#token={TOKEN}"
        );
        for prefix in [
            "https://phux.sh/connect",
            "https://phux.phall.io/connect",
            "phux://connect",
        ] {
            let url = format!("{prefix}?{query}");
            let offer = request(&url).expect(prefix);
            assert_eq!(offer.peer, "wss://mini.tail.ts.net:8787");
            let add = offer.request;
            assert_eq!(add.target, "mini.tail.ts.net");
            assert_eq!(
                add.code.as_ref().map(|code| code.0.as_str()),
                Some(url.as_str())
            );
            assert!(!add.tailnet && add.name.is_none());
        }
    }

    #[test]
    fn a_relay_link_is_named_by_its_route() {
        let ca = format!("sha256:{FP}");
        let relay = format!(
            "https://phux.sh/connect?quic=quic://relay.example:4433&sni=mini-route&fp={FP}&ca={ca}#enroll={TOKEN}&token={TOKEN}"
        );
        assert_eq!(target(&relay).as_deref(), Ok("mini-route"));
        let offer = request(&relay).expect("relay");
        assert_eq!(offer.peer, "mini-route via quic://relay.example:4433");
        assert!(!format!("{offer:?}").contains(TOKEN));
        let named = relay.replace("&fp=", "&name=Paul%27s%20Mac%20mini&fp=");
        assert_eq!(target(&named).as_deref(), Ok("Paul-s-Mac-mini"));
    }

    #[test]
    fn a_name_that_cant_be_typed_falls_back_to_the_host() {
        let url = |name: &str| {
            format!("phux://connect?quic=quic://[fd7a::1]:8788&name={name}&fp={FP}#token={TOKEN}")
        };
        // An IPv6 host has colons; it is still a name once they are dashes.
        assert_eq!(target(&url("%40%40")).as_deref(), Ok("fd7a--1"));
        assert_eq!(target(&url("..")).as_deref(), Ok("fd7a--1"));
        assert_eq!(target(&url("a%2Fb")).as_deref(), Ok("a-b"));
    }

    #[test]
    fn a_bad_link_says_why_without_repeating_it() {
        for url in [
            "phux://connect?url=wss://mini:8787".to_owned(),
            format!("phux://connect/x?url=wss://mini:8787&token={TOKEN}"),
            format!("phux://pair?url=wss://mini:8787&token={TOKEN}"),
            format!("https://evil.example/connect?url=wss://mini:8787&token={TOKEN}"),
            format!("phux://connect?url=http://mini&token={TOKEN}"),
        ] {
            let why = target(&url).expect_err(&url);
            assert!(why.starts_with("This pairing link can't be used"), "{why}");
            assert!(!why.contains(TOKEN), "{why}");
        }
    }

    #[test]
    fn only_a_scheme_argument_is_a_link() {
        assert!(is_link("phux://connect?x=1"));
        assert!(is_link("https://phux.sh/connect?x=1"));
        assert!(!is_link("--session"));
        assert!(!is_link("work"));
    }
}
