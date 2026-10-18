//! The ACME client's HTTP: one HTTP/1.1 request per TLS connection, read to
//! the close. An ACME exchange is a handful of small JSON requests, so this
//! is all it needs; the same shape as phux-server's push client.

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::AcmeError;

/// The largest response read: directories, orders, and certificate chains
/// are a few KiB.
const MAX_RESPONSE: usize = 1024 * 1024;

/// How long one exchange may take, connect to last byte.
const EXCHANGE_DEADLINE: Duration = Duration::from_secs(30);

/// One HTTP response.
#[derive(Debug)]
pub(crate) struct Response {
    pub(crate) status: u16,
    /// Header names lowercased.
    headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
}

impl Response {
    /// The first value of header `name` (lowercase).
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// The body as JSON.
    pub(crate) fn json(&self) -> Result<serde_json::Value, AcmeError> {
        serde_json::from_slice(&self.body)
            .map_err(|err| AcmeError::Protocol(format!("response is not JSON: {err}")))
    }
}

/// An `https://host[:port]/path` URL, split.
struct Url<'a> {
    host: &'a str,
    port: u16,
    path: &'a str,
}

impl Url<'_> {
    /// The `Host` header: the port too unless it is 443 (RFC 9110 §7.2).
    /// A CA builds its directory URLs from it, so dropping the port would
    /// send every later request to the wrong one.
    fn host_header(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.to_owned()
        };
        if self.port == 443 {
            host
        } else {
            format!("{host}:{}", self.port)
        }
    }
}

fn parse_url(url: &str) -> Result<Url<'_>, AcmeError> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| AcmeError::Protocol(format!("not an https URL: {url}")))?;
    let (authority, path) = rest.find('/').map_or((rest, "/"), |at| rest.split_at(at));
    let bad = || AcmeError::Protocol(format!("bad host or port in {url}"));
    let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
        let (host, after) = v6.split_once(']').ok_or_else(bad)?;
        (host, after.strip_prefix(':'))
    } else {
        authority
            .rsplit_once(':')
            .map_or((authority, None), |(host, port)| (host, Some(port)))
    };
    let port = port.map_or(Ok(443), |port| port.parse().map_err(|_| bad()))?;
    if host.is_empty() {
        return Err(AcmeError::Protocol(format!("no host in {url}")));
    }
    Ok(Url { host, port, path })
}

/// The `Accept` an ACME request sends for a JSON object.
pub(crate) const JSON: &str = "application/json";

/// `method url`, with `body` as `application/jose+json` when given, asking
/// for `accept`, trusting the server's certificate to `roots`.
pub(crate) async fn request(
    roots: &Arc<rustls::RootCertStore>,
    method: &str,
    url: &str,
    body: Option<&[u8]>,
    accept: &str,
) -> Result<Response, AcmeError> {
    tokio::time::timeout(
        EXCHANGE_DEADLINE,
        exchange(roots, method, url, body, accept),
    )
    .await
    .map_err(|_| AcmeError::Http(format!("{method} {url}: timed out")))?
}

async fn exchange(
    roots: &Arc<rustls::RootCertStore>,
    method: &str,
    url: &str,
    body: Option<&[u8]>,
    accept: &str,
) -> Result<Response, AcmeError> {
    let parsed = parse_url(url)?;
    let http = |err: std::io::Error| AcmeError::Http(format!("{method} {url}: {err}"));
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|err| AcmeError::Http(err.to_string()))?
    .with_root_certificates(Arc::clone(roots))
    .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from(parsed.host.to_owned())
        .map_err(|err| AcmeError::Protocol(format!("bad host in {url}: {err}")))?;
    let tcp = tokio::net::TcpStream::connect((parsed.host, parsed.port))
        .await
        .map_err(http)?;
    let mut tls = tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(name, tcp)
        .await
        .map_err(http)?;
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: quictun\r\n\
         Accept: {accept}\r\nConnection: close\r\n",
        path = parsed.path,
        host = parsed.host_header(),
    );
    if let Some(body) = body {
        head.push_str("Content-Type: application/jose+json\r\nContent-Length: ");
        head.push_str(&body.len().to_string());
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    tls.write_all(head.as_bytes()).await.map_err(http)?;
    if let Some(body) = body {
        tls.write_all(body).await.map_err(http)?;
    }
    tls.flush().await.map_err(http)?;
    let mut raw = Vec::new();
    let read = (&mut tls)
        .take(u64::try_from(MAX_RESPONSE + 1).unwrap_or(u64::MAX))
        .read_to_end(&mut raw)
        .await;
    // A server may close without TLS close_notify after a whole response,
    // which a length or the chunk framing proves; only a body read to a
    // clean close may stand without either.
    let clean_close = match read {
        Ok(_) => true,
        Err(_) if !raw.is_empty() => false,
        Err(err) => return Err(http(err)),
    };
    if raw.len() > MAX_RESPONSE {
        return Err(AcmeError::Http(format!(
            "{method} {url}: response too large"
        )));
    }
    parse_response(&raw, clean_close)
        .ok_or_else(|| AcmeError::Http(format!("{method} {url}: malformed or cut-off response")))
}

/// Status line, headers, and body: by `Content-Length`, chunked, or (only
/// after a `clean_close`) to the close.
fn parse_response(raw: &[u8], clean_close: bool) -> Option<Response> {
    let split = raw.windows(4).position(|window| window == b"\r\n\r\n")?;
    let head = std::str::from_utf8(&raw[..split]).ok()?;
    let rest = &raw[split + 4..];
    let mut lines = head.split("\r\n");
    let status = lines.next()?.split_whitespace().nth(1)?.parse().ok()?;
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let find = |name: &str| headers.iter().find(|(key, _)| key == name).map(|(_, v)| v);
    let body = if find("transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked")) {
        dechunk(rest)?
    } else if let Some(len) = find("content-length") {
        rest.get(..len.parse::<usize>().ok()?)?.to_vec()
    } else if clean_close {
        rest.to_vec()
    } else {
        return None;
    };
    Some(Response {
        status,
        headers,
        body,
    })
}

/// A chunked body, decoded.
fn dechunk(mut rest: &[u8]) -> Option<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let line_end = rest.windows(2).position(|w| w == b"\r\n")?;
        let size_field = std::str::from_utf8(&rest[..line_end]).ok()?;
        let size = usize::from_str_radix(size_field.split(';').next()?.trim(), 16).ok()?;
        rest = &rest[line_end + 2..];
        if size == 0 {
            return Some(body);
        }
        body.extend_from_slice(rest.get(..size)?);
        rest = rest.get(size + 2..)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_split_into_host_port_and_path() {
        let url = parse_url("https://acme.test:14000/dir").unwrap();
        assert_eq!((url.host, url.port, url.path), ("acme.test", 14000, "/dir"));
        let url = parse_url("https://acme-v02.api.letsencrypt.org/directory").unwrap();
        assert_eq!((url.host, url.port), ("acme-v02.api.letsencrypt.org", 443));
        let url = parse_url("https://[::1]:8443/x").unwrap();
        assert_eq!((url.host, url.port, url.path), ("::1", 8443, "/x"));
        assert_eq!(url.host_header(), "[::1]:8443");
        assert_eq!(
            parse_url("https://ca.test:14000/d").unwrap().host_header(),
            "ca.test:14000"
        );
        assert_eq!(
            parse_url("https://ca.test/d").unwrap().host_header(),
            "ca.test"
        );
        assert!(parse_url("http://acme.test/dir").is_err(), "https only");
    }

    #[test]
    fn responses_parse_by_length_chunks_or_close() {
        let fixed = parse_response(
            b"HTTP/1.1 201 Created\r\nReplay-Nonce: abc\r\nContent-Length: 2\r\n\r\n{}extra",
            false,
        )
        .unwrap();
        assert_eq!(fixed.status, 201);
        assert_eq!(fixed.header("replay-nonce"), Some("abc"));
        assert_eq!(fixed.body, b"{}");

        let chunked = parse_response(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2;x=y\r\nde\r\n0\r\n\r\n",
            false,
        )
        .unwrap();
        assert_eq!(chunked.body, b"abcde");

        let to_close = parse_response(b"HTTP/1.1 200 OK\r\n\r\nall of it", true).unwrap();
        assert_eq!(to_close.body, b"all of it");
        assert!(
            parse_response(b"HTTP/1.1 200 OK\r\n\r\nall of i", false).is_none(),
            "an unframed body cut off by an error is not a response"
        );
        assert!(
            parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nall of", true).is_none(),
            "shorter than its length"
        );
        assert!(parse_response(b"garbage", true).is_none());
    }
}
