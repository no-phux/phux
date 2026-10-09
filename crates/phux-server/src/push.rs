//! Host-originated background push (ADR-0155).
//!
//! A phone that wants to hear about an ask while it is suspended registers a
//! *push grant* under `phux.push/v1/<device>` ([L3](../../../docs/spec/L3.md)
//! §3.11): the gateway it trusts, a bearer secret that gateway minted for it,
//! and the id the phone knows this server by. When an agent asks and the
//! connection that wrote a grant is gone, the server posts a content-free
//! notice (host, pane, question id, action) to that gateway, which forwards it
//! to APNs. The server never holds APNs credentials; the gateway never sees
//! terminal bytes; a phone that is still connected gets nothing here, because
//! it already projects the ask itself.

#![allow(
    clippy::redundant_pub_crate,
    reason = "private server module shared by sibling runtime/state modules"
)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::wire::frame::{PUSH_GRANT_KEY_PREFIX, Scope};
use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::{debug, warn};

use crate::state::{ClientId, ServerState, SharedState};

/// One round trip to the gateway, connect included.
const SEND_TIMEOUT: Duration = Duration::from_secs(10);
/// Registered devices per server. More is a misbehaving client, not a fleet.
const MAX_GRANTS: usize = 32;
const MIN_GRANT_BYTES: usize = 16;
const MAX_GRANT_BYTES: usize = 256;
const MAX_HOST_BYTES: usize = 128;
const MAX_GATEWAY_BYTES: usize = 512;

/// The value a device writes under its grant key.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct GrantValue {
    /// Where to post: `https://...`, or `http://` on loopback only.
    pub(crate) gateway: String,
    /// The bearer secret the gateway minted for this device.
    pub(crate) grant: String,
    /// The id the device knows this server by; echoed so a tap routes.
    pub(crate) host: String,
}

/// Whether a metadata write names a push grant.
pub(crate) fn is_grant_key(scope: &Scope, key: &str) -> bool {
    matches!(scope, Scope::Global) && key.starts_with(PUSH_GRANT_KEY_PREFIX)
}

/// Parse and bound a grant value; a refusal names what is wrong.
pub(crate) fn parse_grant(value: &[u8]) -> Result<GrantValue, String> {
    let grant: GrantValue = serde_json::from_slice(value).map_err(|err| {
        format!("push grant is not a JSON object with gateway, grant, host: {err}")
    })?;
    if grant.grant.len() < MIN_GRANT_BYTES || grant.grant.len() > MAX_GRANT_BYTES {
        return Err(format!(
            "push grant secret must be {MIN_GRANT_BYTES}..={MAX_GRANT_BYTES} bytes"
        ));
    }
    if !grant
        .grant
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("push grant secret must be URL-safe ASCII".to_owned());
    }
    if grant.host.is_empty() || grant.host.len() > MAX_HOST_BYTES {
        return Err(format!(
            "push grant host must be 1..={MAX_HOST_BYTES} bytes"
        ));
    }
    if grant.gateway.len() > MAX_GATEWAY_BYTES {
        return Err(format!(
            "push gateway must be at most {MAX_GATEWAY_BYTES} bytes"
        ));
    }
    Gateway::parse(&grant.gateway)?;
    Ok(grant)
}

/// A gateway URL, reduced to what one POST needs.
#[derive(Debug, PartialEq, Eq)]
struct Gateway {
    tls: bool,
    host: String,
    port: u16,
    path: String,
}

impl Gateway {
    /// `https://host[:port][/path]`, or `http://` when the host is loopback
    /// (a local gateway under test). No userinfo, no empty host.
    fn parse(url: &str) -> Result<Self, String> {
        let (tls, rest) = if let Some(rest) = url.strip_prefix("https://") {
            (true, rest)
        } else if let Some(rest) = url.strip_prefix("http://") {
            (false, rest)
        } else {
            return Err("push gateway must be an https:// URL".to_owned());
        };
        let (authority, path) = rest
            .find('/')
            .map_or((rest, "/"), |at| (&rest[..at], &rest[at..]));
        if authority.is_empty() || authority.contains('@') {
            return Err("push gateway URL has no usable host".to_owned());
        }
        let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
            let (host, after) = bracketed
                .split_once(']')
                .ok_or_else(|| "push gateway URL has an unclosed IPv6 host".to_owned())?;
            (host, after.strip_prefix(':'))
        } else {
            match authority.rsplit_once(':') {
                Some((host, port)) => (host, Some(port)),
                None => (authority, None),
            }
        };
        let port = match port {
            Some(port) => port
                .parse::<u16>()
                .map_err(|_| "push gateway URL has an invalid port".to_owned())?,
            None if tls => 443,
            None => 80,
        };
        if host.is_empty() {
            return Err("push gateway URL has no usable host".to_owned());
        }
        if !tls && !matches!(host, "127.0.0.1" | "::1" | "localhost") {
            return Err("push gateway must be https (http is allowed only on loopback)".to_owned());
        }
        Ok(Self {
            tls,
            host: host.to_owned(),
            port,
            path: path.to_owned(),
        })
    }
}

/// Which connection wrote each grant key, and the last ask each key was
/// pushed for. The grant values themselves live in the metadata store, so a
/// device lists, reads, and deletes its own key as ordinary L3.
#[derive(Debug, Default)]
pub(crate) struct PushGrants {
    setters: HashMap<String, ClientId>,
    last_ask: HashMap<String, String>,
}

impl PushGrants {
    /// Record who wrote `key`. False when the registry is full and `key`
    /// is new; a rewrite of an existing key always lands.
    pub(crate) fn register(&mut self, key: &str, client: ClientId) -> bool {
        if !self.setters.contains_key(key) && self.setters.len() >= MAX_GRANTS {
            return false;
        }
        self.setters.insert(key.to_owned(), client);
        true
    }

    /// The key was deleted.
    pub(crate) fn forget(&mut self, key: &str) {
        self.setters.remove(key);
        self.last_ask.remove(key);
    }

    /// Whether `ask_id` on `key` has not been pushed yet; records it.
    fn claim(&mut self, key: &str, ask_id: &str) -> bool {
        if self.last_ask.get(key).is_some_and(|last| last == ask_id) {
            return false;
        }
        self.last_ask.insert(key.to_owned(), ask_id.to_owned());
        true
    }
}

/// The grants to push `ask_id` to: every registered device whose writing
/// connection is gone, once per ask id. A key whose value no longer parses
/// (rewritten by hand) is skipped, not refused.
pub(crate) fn targets(s: &mut ServerState, ask_id: &str) -> Vec<GrantValue> {
    let keys: Vec<String> = s
        .metadata()
        .list(&Scope::Global)
        .into_iter()
        .filter(|key| key.starts_with(PUSH_GRANT_KEY_PREFIX))
        .collect();
    let mut out = Vec::new();
    for key in keys {
        let Some(grant) = s
            .metadata()
            .get(&Scope::Global, &key)
            .and_then(|value| parse_grant(&value).ok())
        else {
            continue;
        };
        let live = s
            .push_grants()
            .setters
            .get(&key)
            .is_some_and(|client| s.client_connection_cancellation(*client).is_some());
        if live || !s.push_grants_mut().claim(&key, ask_id) {
            continue;
        }
        out.push(grant);
    }
    out
}

/// An ask was broadcast for `pane`: post it to every absent device. Each
/// send is its own task on the server's `LocalSet`; nothing here waits.
pub(crate) fn on_asked(state: &SharedState, pane: &WireResourceId, ask_id: &str) {
    let targets = state.with_mut(|s| targets(s, ask_id));
    if targets.is_empty() {
        return;
    }
    let pane = encode_pane(pane);
    for target in targets {
        let pane = pane.clone();
        let ask_id = ask_id.to_owned();
        tokio::task::spawn_local(async move {
            match send(&target, &pane, &ask_id).await {
                Ok(status) if (200..300).contains(&status) => {
                    debug!(host = %target.host, %pane, "push: delivered to gateway");
                }
                Ok(status) => warn!(host = %target.host, status, "push: gateway refused"),
                Err(err) => warn!(host = %target.host, %err, "push: send failed"),
            }
        });
    }
}

/// The spelling the `UniFFI` binding hands a phone for a terminal
/// (`phux-client-ffi::projection::id::encode`), so the pane in a push is the
/// key the phone already holds. The contract is in L3 §3.11.
fn encode_pane(id: &WireResourceId) -> String {
    match id {
        WireResourceId::Local { id } => format!("local:{id}"),
        WireResourceId::Satellite { host, id } => format!("satellite:{}:{id}", host.as_str()),
    }
}

/// One `POST` of the content-free notice; the HTTP status on success.
///
/// A hand-rolled HTTP/1.1 exchange over `tokio-rustls`: one request, one
/// status line, `Connection: close`. The server has no other HTTP client
/// and this is the only call.
/// ponytail: no redirects, no keep-alive, no body parse; switch to a real
/// client if the gateway contract grows past one POST.
pub(crate) async fn send(target: &GrantValue, pane: &str, question: &str) -> Result<u16, String> {
    let gateway = Gateway::parse(&target.gateway)?;
    let body = serde_json::json!({
        "schema_version": 1,
        "host": target.host,
        "pane": pane,
        "question": question,
        "action": "reply",
    })
    .to_string();
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {grant}\r\n\
         Content-Type: application/json\r\nContent-Length: {len}\r\n\
         User-Agent: phux-server/{version}\r\nConnection: close\r\n\r\n{body}",
        path = gateway.path,
        host = gateway.host,
        grant = target.grant,
        len = body.len(),
        version = env!("CARGO_PKG_VERSION"),
    );
    tokio::time::timeout(SEND_TIMEOUT, async {
        let tcp = TcpStream::connect((gateway.host.as_str(), gateway.port))
            .await
            .map_err(|err| format!("connect {}:{}: {err}", gateway.host, gateway.port))?;
        if gateway.tls {
            let name = rustls::pki_types::ServerName::try_from(gateway.host.clone())
                .map_err(|err| format!("gateway host is not a valid TLS name: {err}"))?;
            let mut tls = connector()?
                .connect(name, tcp)
                .await
                .map_err(|err| format!("TLS to {}: {err}", gateway.host))?;
            exchange(&mut tls, &request).await
        } else {
            let mut tcp = tcp;
            exchange(&mut tcp, &request).await
        }
    })
    .await
    .map_err(|_| {
        format!(
            "no reply from {} within {}s",
            gateway.host,
            SEND_TIMEOUT.as_secs()
        )
    })?
}

fn connector() -> Result<tokio_rustls::TlsConnector, String> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .map_err(|err| format!("build TLS client config: {err}"))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    Ok(tokio_rustls::TlsConnector::from(Arc::new(config)))
}

/// Write `request`, read the status line, and return its code.
async fn exchange<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    request: &str,
) -> Result<u16, String> {
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|err| format!("write: {err}"))?;
    let mut head = Vec::with_capacity(256);
    let mut buf = [0u8; 256];
    while !head.windows(2).any(|w| w == b"\r\n") {
        let n = stream
            .read(&mut buf)
            .await
            .map_err(|err| format!("read: {err}"))?;
        if n == 0 {
            break;
        }
        head.extend_from_slice(&buf[..n]);
        if head.len() > 4096 {
            break;
        }
    }
    let line = String::from_utf8_lossy(&head);
    let line = line.lines().next().unwrap_or_default();
    let status = line
        .strip_prefix("HTTP/1.")
        .and_then(|rest| rest.get(2..5))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| format!("not an HTTP status line: {line:?}"))?;
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn value(gateway: &str) -> Vec<u8> {
        format!(r#"{{"gateway":"{gateway}","grant":"0123456789abcdef","host":"mini"}}"#)
            .into_bytes()
    }

    #[test]
    fn a_grant_is_bounded_and_its_gateway_is_https_or_loopback() {
        assert!(parse_grant(&value("https://push.example/v1/push")).is_ok());
        assert!(parse_grant(&value("http://127.0.0.1:8080/push")).is_ok());
        assert!(parse_grant(&value("http://[::1]:8080/push")).is_ok());
        assert!(parse_grant(&value("http://push.example/push")).is_err());
        assert!(parse_grant(&value("ftp://push.example")).is_err());
        assert!(parse_grant(&value("https://user@push.example")).is_err());
        assert!(parse_grant(br#"{"gateway":"https://x","grant":"short","host":"m"}"#).is_err());
        assert!(
            parse_grant(br#"{"gateway":"https://x","grant":"0123456789abcdef","host":""}"#)
                .is_err()
        );
        assert!(parse_grant(b"not json").is_err());
        let gateway = Gateway::parse("https://push.example/v1/push").expect("parses");
        assert_eq!((gateway.port, gateway.path.as_str()), (443, "/v1/push"));
    }

    /// A connected phone projects the ask itself; an absent one is pushed,
    /// once per ask id, and a new question pushes again.
    #[test]
    fn only_absent_devices_are_pushed_and_only_once_per_ask() {
        let mut s = ServerState::new();
        let key = format!("{PUSH_GRANT_KEY_PREFIX}phone-1");
        s.metadata_set(&Scope::Global, &key, value("https://push.example/v1/push"));
        assert!(s.push_grants_mut().register(&key, ClientId(7)));
        s.set_client_connection_cancellation(
            ClientId(7),
            tokio_util::sync::CancellationToken::new(),
        );
        assert!(
            targets(&mut s, "q1").is_empty(),
            "a live setter is not pushed"
        );

        s.forget_connection(ClientId(7));
        assert_eq!(targets(&mut s, "q1").len(), 1, "an absent setter is pushed");
        assert!(
            targets(&mut s, "q1").is_empty(),
            "the same ask is not pushed twice"
        );
        assert_eq!(targets(&mut s, "q2").len(), 1, "a new ask pushes again");

        s.push_grants_mut().forget(&key);
        s.metadata_delete(&Scope::Global, &key);
        assert!(targets(&mut s, "q3").is_empty());
    }

    #[test]
    fn the_registry_is_capped_but_rewrites_always_land() {
        let mut grants = PushGrants::default();
        for n in 0..MAX_GRANTS {
            assert!(grants.register(&format!("k{n}"), ClientId(1)));
        }
        assert!(!grants.register("one-too-many", ClientId(1)));
        assert!(grants.register("k0", ClientId(2)));
    }

    /// The exchange a gateway sees: bearer grant, JSON body with the phone's
    /// own host id, the pane in the binding's spelling, and the ask id.
    #[tokio::test(flavor = "current_thread")]
    async fn a_send_posts_the_content_free_notice_with_the_grant_as_bearer() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let captured = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().expect("accept");
            let mut raw = Vec::new();
            let mut buf = [0u8; 1024];
            loop {
                let n = conn.read(&mut buf).expect("read");
                raw.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&raw);
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let len: usize = head
                        .lines()
                        .find_map(|l| l.strip_prefix("Content-Length: "))
                        .and_then(|v| v.parse().ok())
                        .expect("content length");
                    if body.len() >= len || n == 0 {
                        break;
                    }
                }
            }
            conn.write_all(
                b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .expect("reply");
            String::from_utf8_lossy(&raw).into_owned()
        });
        let target = GrantValue {
            gateway: format!("http://127.0.0.1:{port}/v1/push"),
            grant: "0123456789abcdef".to_owned(),
            host: "mini".to_owned(),
        };
        let status = send(&target, "local:7", "q1").await.expect("sent");
        assert_eq!(status, 202);
        let request = captured.join().expect("gateway thread");
        assert!(
            request.starts_with("POST /v1/push HTTP/1.1\r\n"),
            "{request}"
        );
        assert!(request.contains("Authorization: Bearer 0123456789abcdef\r\n"));
        let body = request.split("\r\n\r\n").nth(1).expect("body");
        let json: serde_json::Value = serde_json::from_str(body).expect("json body");
        assert_eq!(json["host"], "mini");
        assert_eq!(json["pane"], "local:7");
        assert_eq!(json["question"], "q1");
        assert_eq!(json["action"], "reply");
        assert!(
            json.get("text").is_none(),
            "no question text leaves the host"
        );
    }

    #[test]
    fn panes_are_spelled_as_the_binding_spells_them() {
        assert_eq!(encode_pane(&WireResourceId::local(7)), "local:7");
        assert_eq!(
            encode_pane(&WireResourceId::satellite("box", 3)),
            "satellite:box:3"
        );
    }
}
