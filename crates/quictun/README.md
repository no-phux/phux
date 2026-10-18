# quictun

A blind QUIC tunnel relay. A service behind NAT dials **out** to the relay
and holds one tunnel for a named route. Consumers dial **in** and name the
route in the TLS server name. The relay opens one QUIC stream over the
tunnel per consumer and splices the two without reading them.

The relay routes on one label and forwards bytes. If the consumer and the
service run their own TLS session inside the stream (they should), the
relay sees only ciphertext and some metadata: addresses, the route, and
record sizes and timing. It holds no key for the session.

## Why QUIC

The usual design is TCP over a WebSocket bridge. QUIC improves on it:

- **No head-of-line blocking across consumers.** Each consumer is its own
  stream on the tunnel, with its own flow-control window, so one stalled
  consumer never holds back another.
- **Bounded lag.** The relay holds each consumer's send window to the
  path's congestion window (`window`), and it gives each tunnel stream a
  small receive window, so a slow consumer pushes back on the service
  within about a round trip instead of queueing seconds of data.
- **Survives roaming.** QUIC connection migration keeps a tunnel alive
  when the service's address changes.
- **One endpoint.** Tunnels and consumers share one UDP socket and one
  handshake budget. The leg is decided by the negotiated ALPN, never by
  what a peer sends.

## Shape

| Piece | What it is |
|---|---|
| `Protocol` | The tunnel ALPN, the consumer ALPNs, and the connection-ID tag. Nothing in the crate names an application. |
| `tunnel` | The wire: the stream-0 token preamble, the close codes, the tagged connection ID. |
| `connect` | The client legs: a service's `Tunnel` and a consumer's `dial_route`, pinning the relay's leaf. |
| `relay` | The relay: the endpoint, the TLS-layer route gate, the route-token store, and the splice (feature `relay`, default). |
| `pin` | Leaf-certificate pinning by SHA-256 fingerprint. |
| `window` | A congestion-tracked QUIC send window for any quinn writer. |
| `cert`, `secret_file` | Self-signed certificates and owner-only secret files (feature `provision`). |

With `default-features = false` you get the wire, the client legs, pinning,
and the send window. That is what a client build needs.

## Example

```rust,no_run
use quictun::connect::{RelayAddr, Tunnel, dial_route};
use quictun::relay::{RelayConfig, RelayRuntime, cert_fingerprint, mint_route_token};
use quictun::{CidTag, Protocol};

# async fn demo() -> Result<(), Box<dyn std::error::Error>> {
let protocol = Protocol::new(b"myapp-tunnel/1", b"myapp/1", CidTag(*b"myaT"));

// The relay: three files in a state directory, one UDP port.
let dir = quictun::relay::state_dir("myapp-relay");
let config = RelayConfig::in_dir("0.0.0.0:4433".parse()?, &dir, protocol.clone());
let token = hex::decode(mint_route_token(&config.tokens_path, "laptop")?)?;
let pin = { quictun::relay::ensure_self_signed(&config.cert_path, &config.key_path)?;
            cert_fingerprint(&config.cert_path)? };
tokio::spawn(RelayRuntime::new(config).run_async(std::future::pending()));

// The service: dial out once, then take one stream per consumer.
let relay = RelayAddr { addr: "203.0.113.7:4433".parse()?, pin };
let tunnel = Tunnel::open(&relay, &protocol, &token).await?;
tokio::spawn(async move {
    while let Ok((_send, _recv)) = tunnel.accept().await {
        // Run your TLS server here, over this stream.
    }
});

// A consumer: name the route in the server name.
let (_endpoint, _conn, _send, _recv) = dial_route(&relay, "laptop", b"myapp/1").await?;
# Ok(()) }
```

## Operating it

- **Routes** are lowercase DNS labels. They are not secrets and not
  authentication.
- **Route tokens** are 256-bit values, stored one per line as
  `<64-char hex> <route>` in an owner-only file. To revoke one, delete
  its line. The relay re-reads the file whenever it changes.
- **A newer tunnel for a route replaces the old one** (last-writer-wins).
  The old tunnel is closed with `RECLAIMED`.
- **Unknown routes are refused during the TLS handshake**, before any
  application byte flows. An enrolled route with no live tunnel is closed
  with `ROUTE_OFFLINE`.
- **A bridge ends when either direction finishes.** A consumer that
  half-closes its stream before it has read the reply loses the reply.

## Status

quictun is phux's relay ([ADR-0181]). It depends on no phux crate. Its wire
is stable within a major version.

Licensed under Apache-2.0.

[ADR-0181]: https://github.com/no-phux/phux/blob/main/docs/adr/0181-the-relay-is-a-standalone-quic-tunnel-crate.md
