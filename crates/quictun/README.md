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

## Quick start

Expose a local web server on a laptop behind NAT, then reach it from
another machine:

```sh
# On the relay host (a VPS with a public UDP port):
quictun pair --route laptop          # prints the route token and the relay pin
quictun relay --listen 0.0.0.0:4433  # prints the relay pin again

# On the laptop, with the token saved to an owner-only file:
quictun expose --relay relay.example.com:4433 --relay-pin <RELAY_PIN> \
               --token-file ~/.laptop-token --to 127.0.0.1:8080
                                     # prints the service pin

# Anywhere:
quictun connect --relay relay.example.com:4433 --relay-pin <RELAY_PIN> \
                --route laptop --service-pin <SERVICE_PIN> --listen 127.0.0.1:9000
curl http://127.0.0.1:9000/
```

`expose` terminates TLS on every bridged stream and `connect` pins its
certificate, so even a plaintext service crosses the relay as ciphertext.
With `--passthrough` on both ends the stream is forwarded raw, for a
target that runs TLS itself.

`connect` carries every local connection as its own stream on one QUIC
connection to the relay: one handshake, and no head-of-line blocking
between them.

**Browsers and other clients without QUIC:** run the relay with
`--tcp-listen 0.0.0.0:443 --domain tunnel.example.com` and point
`*.tunnel.example.com` at it. The relay reads only the TLS ClientHello's
server name, routes `laptop.tunnel.example.com` to route `laptop`, and
forwards the encrypted bytes; TLS still ends at the laptop, which needs a
certificate browsers trust. Let `expose` get one from Let's Encrypt:

```sh
quictun expose --relay relay.example.com:4433 --relay-pin <RELAY_PIN> \
               --token-file ~/.laptop-token --to 127.0.0.1:8080 \
               --acme-domain laptop.tunnel.example.com --acme-email you@example.com
curl https://laptop.tunnel.example.com/
```

Validation is TLS-ALPN-01: the CA connects to the name on port 443, the
relay routes that connection to the laptop like any other, and the laptop
answers. There is no inbound port at the laptop and no change at the
relay, and the certificate's key never leaves the laptop. It is renewed
30 days before it expires. `connect --service-name laptop.tunnel.example.com`
then verifies the service the way a browser does, so renewal never breaks
a pin. `--cert` and `--key` still take any certificate you already have.

## Shape

| Piece | What it is |
|---|---|
| `Protocol` | The tunnel ALPN, the consumer ALPNs, the connection-ID tag, the half-close rule, and how many streams one consumer connection may have bridged. Nothing in the crate names an application; `Protocol::quictun()` is the binary's. |
| `tunnel` | The wire: the stream-0 token preamble, the close codes, the tagged connection ID. |
| `connect` | The client legs: a service's `Tunnel` and a consumer's `dial_route`, pinning the relay's leaf. |
| `relay` | The relay: the endpoint, the TLS-layer route gate, the route-token store, per-route caps, the optional TCP ingress, and the splice (feature `relay`, default). |
| `forward` | TCP forwarding with TLS at the service: `expose` and `connect` (feature `forward`, default). |
| `acme` | Public certificates for an exposed service by ACME TLS-ALPN-01, through the relay (feature `acme`, default). |
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
- **On QUIC, unknown routes are refused during the TLS handshake**, before
  any application byte flows, and an enrolled route with no live tunnel is
  closed with `ROUTE_OFFLINE`. **On the TCP ingress** the relay just drops
  the socket after reading the ClientHello: no alert, no code.
- **Stalled clients cannot take the relay.** Until a QUIC peer finishes its
  handshake, or a TCP peer's ClientHello names a live route, it holds only
  a handshake permit and its address's share, never a connection slot.
  Bridged TCP sockets use keepalive, so a peer that vanished frees its
  slot.
- **Half-close is the protocol's choice.** `HalfClose::Propagate` (the
  binary's) forwards each FIN and keeps the other direction flowing, as
  TCP does. `HalfClose::EndsBridge` (`Protocol::new`'s default) ends the
  bridge when either direction finishes, for protocols whose peers never
  half-close.
- **Tenants share fairly.** `--max-conns` caps the relay, and
  `--max-conns-per-route` caps how many consumers one route may have at
  once (QUIC and TCP together); a refused QUIC consumer is closed with
  `OVER_CAP`. Consumers are unauthenticated at the relay, so anyone who
  knows a route's name can fill its cap; the cap protects the other routes,
  not the route itself. Accounts and billing belong to whatever writes the
  route-token store.

## Status

quictun is phux's relay ([ADR-0181], [ADR-0183], [ADR-0184]). It depends on no phux
crate. Its wire is stable within a major version.

Licensed under Apache-2.0.

[ADR-0181]: https://github.com/no-phux/phux/blob/main/docs/adr/0181-the-relay-is-a-standalone-quic-tunnel-crate.md
[ADR-0183]: https://github.com/no-phux/phux/blob/main/docs/adr/0183-quictun-is-a-standalone-tunnel-product.md
[ADR-0184]: https://github.com/no-phux/phux/blob/main/docs/adr/0184-quictun-gets-acme-certificates-and-multiplexes-consumers.md
