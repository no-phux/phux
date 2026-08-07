---
audience: consumers, contributors, agents
stability: evolving
last-reviewed: 2026-08-07
---

# The phux iOS client

**TL;DR.** phux-mobile is the native edition of the projection-consumer
pattern: a Swift/SwiftUI iOS app that links `phux-protocol` and the VT engine
through a Rust UniFFI bridge and runs the engine on the phone. It is one peer
among many, not protocol-privileged. It reaches a server over `wss://` with a
pinned leaf certificate and a `phux pair` bearer token — the pairing surface
this page exists to pin down, because leaving it unwritten once broke pairing
outright.

---

## What it is

phux-mobile ([github.com/phall1/phux-mobile](https://github.com/phall1/phux-mobile))
consumes the wire the same way [`web.md`](./web.md) does — it carries its own
engine and projects structured screen state locally ([ADR-0030]) — but
natively rather than through WASM. It depends on `phux-protocol` only; it does
not link `phux-server`, and it defines no wire bytes of its own.

Wire parity is rev-for-rev: the app pins the exact `../phux` revision it builds
against in its `PHUX_REV` file, and a CI gate fails on drift.

## Transport

WebSocket only. The bridge uses tokio-tungstenite over rustls; it does not
speak QUIC, so the QUIC ALPN and relay-tunnel material in
[`../spec/proto.md`](../spec/proto.md) §4 and §4.1 does not apply to it today.

Per [ADR-0031], a routable address is always `wss://`. Plaintext `ws://` is
accepted only for loopback — the client refuses a routable `ws://` target
before dialing, matching the server's own refusal, so the failure is legible on
the device rather than a silent hang.

TLS trust is the pinned leaf certificate, not the system roots: the client
compares SHA-256 of the presented leaf against the fingerprint it was paired
with. There is no CA fallback. A `wss://` connection with no pinned
fingerprint fails immediately and terminally rather than retrying, because no
retry can supply a credential the connection does not have.

## Pairing: the contract

This is the part other repos must not guess at.

`phux pair` mints the token and prints a one-tap link. **This repo owns that
link's shape** ([ADR-0031]); consumers accept it. `build_connect_link` in
`crates/phux/src/commands/pair.rs` emits:

```
phux://connect?url=<ws(s)-url>[&name=<pct-encoded>][&fp=<sha256>]&token=<hex>
```

with these properties a consumer may rely on:

- `url` is mandatory and always first. Without it no link is printed at all.
- `token` is **always present and always last**. It is appended
  unconditionally — there is no token-free variant of this link, and
  `--qr` encodes the identical string.
- `name` and `fp` are optional and, when present, appear in that order between
  `url` and `token`. Absent means absent, never empty: no `fp=` at all rather
  than `fp=`.
- `name` is percent-encoded (it is free-form operator input). `url`, `fp` and
  `token` are query-safe as emitted and are **not** percent-encoded — `:` and
  `/` are legal RFC 3986 `pchar`, so a consumer must read them unescaped.
- `fp` is uppercase, colon-separated hex.

A consumer must accept every one of those shapes, including the token. It may
not require a token-free variant, because none exists.

### Consent is the consumer's job, not the link's

The token travels in the URL, so by the time a device sees it the credential
has already been exposed to terminal scrollback, to the QR image, and to
whatever carried the link. A consumer that *refuses* the token gains nothing
against that exposure and loses pairing entirely.

The correct posture, and the one phux-mobile implements, is to accept the
token and require explicit user confirmation before it enters the device's
saved-host list. Any app can open a `phux://` URL, so the confirmation is what
distinguishes "I am pairing this device" from "a web page sent me a host to
trust". Refusing the credential never addressed that second case at all — an
attacker's own server does not need a token to accept a client.

### Failure modes worth handling

- A link that fails to parse must be reported, never silently dropped. A
  no-op is indistinguishable from a broken app.
- `phux pair` prints no link when it cannot determine an address. The device
  then needs a manual entry path.
- When the certificate is unreadable, `phux pair` warns and emits a link with
  no `fp`. A `wss://` target without a pin cannot connect, so the consumer
  should say so rather than dialing.

## Protocol version

The app sends `PROTOCOL_VERSION` from the `phux-protocol` revision it pins and
requires an exact `major.minor` match at `HELLO_OK`. A mismatch is fatal and
not retried, and should be surfaced as a version problem rather than a generic
timeout — the two are frequently confused because a version mismatch and an
unreachable host both end in "nothing happened".

## Related

- [ADR-0031] — remote consumer auth and encryption; the pairing decision record.
- [ADR-0030] — engine-delegated wire and projection consumers.
- [`web.md`](./web.md) — the WASM edition of the same pattern.
- [`../spec/proto.md`](../spec/proto.md) — the normative wire specification.

[ADR-0030]: ../../ADR/0030-engine-delegated-wire-and-projection-consumers.md
[ADR-0031]: ../../ADR/0031-remote-consumer-auth-and-encryption.md
