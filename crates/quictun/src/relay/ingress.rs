//! TCP ingress: consumers that cannot speak QUIC (a browser, `curl`, any TLS
//! client) reach a route over plain TCP.
//!
//! The relay reads the client's TLS `ClientHello` only far enough to learn its
//! server name, maps that name to a route, and forwards the hello and
//! everything after it, unchanged, over a fresh stream on the route's
//! tunnel. TLS ends at the service, so the relay forwards ciphertext here as
//! on the QUIC leg; the service must present a certificate the client
//! trusts for that name. A hello that names no enrolled route with a live
//! tunnel is dropped.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncReadExt as _};
use tokio::net::{TcpListener, TcpStream};

use crate::relay::RelayError;
use crate::relay::runtime::{CONSUMER_STREAM_DEADLINE, Shared};
use crate::splice::{Tee, splice};

/// The largest `ClientHello` the relay buffers before giving up on it.
const MAX_CLIENT_HELLO: usize = 16 * 1024;

/// Where the relay accepts TCP consumers and how it names routes there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TcpIngress {
    /// Address the TCP listener binds.
    pub listen: SocketAddr,
    /// With a domain, a client reaches route `R` by the server name
    /// `R.<domain>` (a wildcard DNS name pointed at the relay); without one,
    /// the server name is the route itself.
    pub domain: Option<String>,
}

/// A bound TCP ingress listener.
#[derive(Debug)]
pub(crate) struct BoundIngress {
    listener: TcpListener,
    domain: Option<Arc<str>>,
}

impl BoundIngress {
    pub(crate) fn local_addr(&self) -> SocketAddr {
        // A bound listener always has a local address; fall back to the
        // unspecified one rather than panic.
        self.listener
            .local_addr()
            .unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], 0)))
    }
}

/// Bind the TCP listener. Must be called within a tokio runtime context.
pub(crate) fn bind(ingress: TcpIngress) -> Result<BoundIngress, RelayError> {
    let listener = std::net::TcpListener::bind(ingress.listen)?;
    listener.set_nonblocking(true)?;
    let domain = ingress
        .domain
        .map(|domain| domain.trim_matches('.').to_ascii_lowercase())
        .filter(|domain| !domain.is_empty())
        .map(Arc::from);
    Ok(BoundIngress {
        listener: TcpListener::from_std(listener)?,
        domain,
    })
}

/// The next TCP consumer and the ingress's domain, or never without an
/// ingress. A failed accept (a reset before accept, or the process out of
/// descriptors) yields `None` and the relay keeps serving.
pub(crate) async fn accept(
    ingress: Option<&BoundIngress>,
) -> Option<(TcpStream, SocketAddr, Option<Arc<str>>)> {
    let Some(ingress) = ingress else {
        return std::future::pending().await;
    };
    match ingress.listener.accept().await {
        Ok((stream, remote)) => Some((stream, remote, ingress.domain.clone())),
        Err(err) => {
            tracing::debug!(%err, "tcp accept failed");
            None
        }
    }
}

/// Route one TCP consumer by its `ClientHello` and splice it onto a fresh
/// stream over the route's tunnel.
///
/// It holds a connection slot throughout, and its source's share until the
/// hello is read, as a QUIC consumer does.
pub(crate) async fn bridge(
    mut stream: TcpStream,
    remote: SocketAddr,
    domain: Option<Arc<str>>,
    shared: Arc<Shared>,
) {
    let Ok(_slot) = Arc::clone(&shared.slots).try_acquire_owned() else {
        tracing::warn!(%remote, "refused tcp: relay at connection cap");
        return;
    };
    // TCP's handshake proves the address, so this is the validated share.
    let Some(source) = shared.sources.try_take((remote.ip(), true)) else {
        tracing::debug!(%remote, "refused tcp: source at its handshake share");
        return;
    };
    let hello = tokio::time::timeout(CONSUMER_STREAM_DEADLINE, read_client_hello(&mut stream))
        .await
        .ok()
        .flatten();
    drop(source);
    let Some((hello, server_name)) = hello else {
        tracing::debug!(%remote, "refused tcp: no TLS `ClientHello` with a server name in time");
        return;
    };
    let Some(route) = route_for(&server_name, domain.as_deref()) else {
        tracing::debug!(%remote, server_name, "refused tcp: server name names no route");
        return;
    };
    if !shared.tokens.current().routes().contains(route) {
        tracing::debug!(%remote, route, "refused tcp: route not enrolled");
        return;
    }
    let Some(tunnel) = shared.registry.get(route) else {
        tracing::warn!(%remote, route, "refused tcp: no live tunnel for enrolled route");
        return;
    };
    let Some(_route_share) = shared.take_route_share(route) else {
        tracing::warn!(%remote, route, "refused tcp: route at its consumer cap");
        return;
    };
    let Ok(Ok((mut tun_send, tun_recv))) =
        tokio::time::timeout(CONSUMER_STREAM_DEADLINE, tunnel.open_bi()).await
    else {
        tracing::warn!(%remote, route, "tunnel dropped while bridging tcp consumer");
        return;
    };
    shared.observe_bytes(&hello);
    if tun_send.write_all(&hello).await.is_err() {
        return;
    }
    let _ = stream.set_nodelay(true);
    tracing::info!(route, %remote, "tcp consumer bridged");
    let observed = &shared.observers.consumer_bytes;
    let (tcp_recv, tcp_send) = stream.into_split();
    splice(
        Tee::new(tcp_recv, observed.clone()),
        tun_send,
        Tee::new(tun_recv, observed.clone()),
        tcp_send,
        shared.protocol.half_close,
    )
    .await;
    tracing::debug!(route, %remote, "tcp consumer bridge ended");
}

/// The route a TLS server name reaches: `R` for `R.<domain>` with a domain,
/// the name itself without one. Only a single label is a route.
pub(crate) fn route_for<'a>(server_name: &'a str, domain: Option<&str>) -> Option<&'a str> {
    let route = match domain {
        Some(domain) => server_name.strip_suffix(domain)?.strip_suffix('.')?,
        None => server_name,
    };
    (!route.is_empty() && !route.contains('.')).then_some(route)
}

/// Read until a whole `ClientHello` has arrived and return every byte read
/// (to forward unchanged) with its server name, lowercased. `None` for a
/// stream that ends first, is not TLS, sends no server name, or exceeds
/// [`MAX_CLIENT_HELLO`].
async fn read_client_hello<R>(stream: &mut R) -> Option<(Vec<u8>, String)>
where
    R: AsyncRead + Unpin,
{
    let mut acceptor = rustls::server::Acceptor::default();
    let mut buffered = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffered.extend_from_slice(&chunk[..read]);
        if buffered.len() > MAX_CLIENT_HELLO {
            return None;
        }
        let mut fresh = &chunk[..read];
        while !fresh.is_empty() {
            if acceptor.read_tls(&mut fresh).ok()? == 0 {
                return None;
            }
        }
        match acceptor.accept() {
            Ok(Some(accepted)) => {
                let name = accepted.client_hello().server_name()?.to_ascii_lowercase();
                return Some((buffered, name));
            }
            Ok(None) => {}
            Err(_) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_names_map_to_single_label_routes() {
        assert_eq!(
            route_for("laptop.relay.dev", Some("relay.dev")),
            Some("laptop")
        );
        assert_eq!(route_for("laptop", None), Some("laptop"));
        assert_eq!(route_for("relay.dev", Some("relay.dev")), None, "no label");
        assert_eq!(
            route_for("a.b.relay.dev", Some("relay.dev")),
            None,
            "two labels"
        );
        assert_eq!(
            route_for("laptopxrelay.dev", Some("relay.dev")),
            None,
            "no dot"
        );
        assert_eq!(route_for("laptop.other.dev", Some("relay.dev")), None);
        assert_eq!(route_for("a.b", None), None);
    }

    /// The bytes a real rustls client sends first, for `name`.
    fn client_hello(name: &str) -> Vec<u8> {
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();
        let name = rustls::pki_types::ServerName::try_from(name.to_owned()).unwrap();
        let mut client = rustls::ClientConnection::new(Arc::new(config), name).unwrap();
        let mut hello = Vec::new();
        client.write_tls(&mut hello).unwrap();
        hello
    }

    #[tokio::test]
    async fn a_client_hello_yields_its_name_and_every_byte_read() {
        let mut hello = client_hello("Laptop.Relay.Dev");
        hello.extend_from_slice(b"after");
        let (bytes, name) = read_client_hello(&mut &hello[..]).await.unwrap();
        assert_eq!(name, "laptop.relay.dev");
        assert_eq!(bytes, hello, "forwarded unchanged, trailing bytes included");
    }

    #[tokio::test]
    async fn anything_but_a_named_client_hello_is_refused() {
        assert!(
            read_client_hello(&mut &b"GET / HTTP/1.1\r\n\r\n"[..])
                .await
                .is_none()
        );
        let hello = client_hello("laptop.relay.dev");
        assert!(
            read_client_hello(&mut &hello[..hello.len() / 2])
                .await
                .is_none(),
            "cut short"
        );
        let ip_only = client_hello("127.0.0.1");
        assert!(
            read_client_hello(&mut &ip_only[..]).await.is_none(),
            "no SNI for an IP"
        );
    }
}
