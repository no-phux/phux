//! The links a pane may ask to have opened on the client's machine
//! (`docs/ssh.md#opening-links`): checked by the server before it asks and
//! again by the client before it shows one.

use phux_protocol::open_url::MAX_OPEN_URL_BYTES;
use url::{Host, Url};

/// An absolute `http` or `https` link with a host and no user name or
/// password.
///
/// Its text is all printable ASCII: a non-ASCII host is in its punycode form
/// and everything else non-ASCII is percent-encoded, so what a prompt shows
/// is exactly what is opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebLink(Url);

impl WebLink {
    /// The link `raw` spells, or why it is refused.
    ///
    /// # Errors
    /// Anything but the links [`WebLink`] describes.
    pub fn parse(raw: &str) -> Result<Self, &'static str> {
        const TOO_LONG: &str = "the link is longer than 8 KiB";
        if raw.len() > MAX_OPEN_URL_BYTES {
            return Err(TOO_LONG);
        }
        if raw.chars().any(char::is_control) {
            return Err("the link holds a control character");
        }
        let url = Url::parse(raw).map_err(|_| "not an absolute link")?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err("only http and https links can be opened");
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err("the link holds a user name or password");
        }
        if url.as_str().len() > MAX_OPEN_URL_BYTES {
            return Err(TOO_LONG);
        }
        if url.host_str().is_none_or(str::is_empty)
            || !url.as_str().bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err("the link has no readable host");
        }
        Ok(Self(url))
    }

    /// The whole link.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Its host, without the port.
    #[must_use]
    pub fn host(&self) -> &str {
        self.0.host_str().unwrap_or_default()
    }

    /// The port of the loopback address a sign-in comes back to: the first
    /// query value that is itself a link to this machine with a port.
    #[must_use]
    pub fn callback_port(&self) -> Option<u16> {
        self.0.query_pairs().find_map(|(_, value)| {
            let back = Url::parse(&value).ok()?;
            let loopback = match back.host()? {
                Host::Domain(name) => name == "localhost",
                Host::Ipv4(ip) => ip.is_loopback(),
                Host::Ipv6(ip) => ip.is_loopback(),
            };
            back.port().filter(|_| loopback)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_web_links_parse_and_they_read_as_they_open() {
        let long = format!("https://a.example/{}", "x".repeat(MAX_OPEN_URL_BYTES));
        let grows = format!("https://a.example/{}", "é".repeat(MAX_OPEN_URL_BYTES / 3));
        for bad in [
            "",
            "a.example",
            "//a.example",
            "https://",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "ssh://a.example",
            "-https://a.example",
            "https://bank.example@evil.example/",
            "https://me:secret@a.example/",
            "https://a.example/\u{1b}[2J",
            "https://a.example/\nx",
            "ht\ttps://a.example/",
            "https://a.example/\u{85}",
            "https://xn--a.example.\u{ffff}/",
            &long,
            &grows,
        ] {
            assert!(WebLink::parse(bad).is_err(), "{bad:?}");
        }
        for (raw, shown, host) in [
            ("https://a.example", "https://a.example/", "a.example"),
            (
                "HTTP://A.example:8080/p?q#f",
                "http://a.example:8080/p?q#f",
                "a.example",
            ),
            // A lookalike host reads as punycode.
            (
                "https://аpple.com/",
                "https://xn--pple-43d.com/",
                "xn--pple-43d.com",
            ),
            // Invisible and direction-changing characters never reach a prompt.
            (
                "https://a.example/\u{202e}gpj\u{200b}",
                "https://a.example/%E2%80%AEgpj%E2%80%8B",
                "a.example",
            ),
            (
                "https:\\\\evil.example\\@a.example",
                "https://evil.example/@a.example",
                "evil.example",
            ),
            ("http://0x7f.1/", "http://127.0.0.1/", "127.0.0.1"),
            ("http://[::1]:3000/", "http://[::1]:3000/", "[::1]"),
        ] {
            let link = WebLink::parse(raw).unwrap_or_else(|why| panic!("{raw:?}: {why}"));
            assert_eq!((link.as_str(), link.host()), (shown, host), "{raw:?}");
            assert_eq!(WebLink::parse(shown), Ok(link), "{shown:?} reads the same");
        }
    }

    #[test]
    fn a_sign_in_that_comes_back_to_loopback_names_its_port() {
        let port = |raw| WebLink::parse(raw).expect("parses").callback_port();
        let back = "https://id.example/auth?client=x&redirect_uri=http%3A%2F%2Flocalhost%3A8085%2F";
        assert_eq!(port(back), Some(8085));
        assert_eq!(
            port("https://id.example/?r=http://127.0.0.1:1455/cb"),
            Some(1455)
        );
        assert_eq!(port("https://id.example/?r=http://[::1]:9/cb"), Some(9));
        assert_eq!(port("https://id.example/?r=https://a.example:8085/"), None);
        assert_eq!(port("https://id.example/?r=http://localhost/"), None);
        assert_eq!(port("http://localhost:3000/"), None);
    }
}
