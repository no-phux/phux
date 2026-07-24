---
audience: contributors
stability: stable
last-reviewed: 2026-07-21
---

# 0057 — A minimal reference relay in-tree

**TL;DR.** phux ships a single-process, single-tenant reference relay —
`phux relay run` / `phux relay pair` — implementing ADR-0051's tunnel
shape and ADR-0052's SNI routing and enrollment binding exactly. A dev
and self-host tool, not infrastructure software: no accounts, no config
file, no persistence beyond a route-bound token store and a keypair.

Status: Proposed
Date: 2026-07-21

## Context

[ADR-0051](./0051-outbound-dial-out-connector-transport.md)'s
trust-honesty claim — self-hosting is the only mitigation for
relay-sees-plaintext — is hollow without a relay anyone can run; bead
phux-b1ma decided in-tree. What existed before: a spike test only
(`relay_connector_spike.rs`), no production `handshake_data` read, no
token store that answers "which route" (ADR-0052 Decision 2 needs that).
Implementation hard-depended on bead phux-zwuz (`QUIC_RELAY_ALPN` in
`phux_protocol::policy` plus the ALPN-parameterized dialer), now landed.
(ADR-0052 is proposed separately and must merge before this ADR; its
mentions here stay plain text until it lands.)

## Decision

1. **Scope fence.** Single-tenant; no accounts, HA, metrics, config file,
   or rate limiting beyond a connection cap. The fence is normative:
   cross-fence requests are answered by this ADR, not triaged.
2. **Home.** New crate `crates/phux-relay`, `publish = false`, library
   only; the verb lives in the `phux` binary (`commands/relay.rs`); zero
   new dependencies. Depends on `phux-protocol` only for the two ALPN
   constants, never on `phux-server` — the crate graph makes "never
   parses phux frames" structural. Sole duplication: the eight-line
   `state_dir` helper (`paths` module), documented in place.
3. **Surface.** Exactly two verbs: `run --listen HOST:PORT` (required,
   no default) with `--max-conns N` (default 64, the sole knob); `pair
   --route NAME` (prints token + cert fingerprint in the `phux pair`
   house style, provisions certs on first use). No revoke/list/status/
   `--json`/path flags/env vars in v1; revocation is deleting a line,
   listing is reading the file.
4. **Files.** Exactly three, fixed XDG state-dir paths — `relay-tokens`
   (`<64-hex> <route>` lines, 0600), `relay-cert.pem`, `relay-key.pem` —
   siblings of the server's `remote-*` files. A new format, not
   `TokenStore`: lookup is constant-time and returns the bound route.
5. **Wire.** One endpoint, two ALPNs, role decided by negotiated ALPN
   only. Connector leg: length-prefixed auth preamble (256-byte bound, 5s
   deadline), then stream-0 silence. Consumer leg: SNI-routed; unknown or
   absent SNI refused at the TLS layer — the certificate resolver
   declines, so the handshake fails with no phux-shaped error. Blind
   splice, including the consumer's own bearer preamble; one bidi stream
   per consumer; 30s idle / 10s keep-alive.
6. **Distinguishable refusals.** Application close codes: AUTH_FAILED
   0x01 (mirrors the server listener), ROUTE_OFFLINE 0x02 (enrolled
   route, no live tunnel — handshake completes, then app-close),
   RECLAIMED 0x03, PROTOCOL_VIOLATION 0x04 (bytes on stream 0 after the
   preamble), OVER_CAP 0x05 (handshake completes, then app-close;
   existing connections unaffected). Normative home: Open Question 1.
7. **Duplicate route claim.** Last-writer-wins: the incumbent tunnel is
   closed RECLAIMED; the warn log is the operator's theft-detection
   surface. First-holder-wins would wedge a redeployed server.
8. **Route names** are lowercase RFC 1123 DNS labels (`[a-z0-9-]`, at
   most 63 chars, no leading/trailing hyphen); `pair` rejects — never
   normalizes — at mint, and the store re-rejects at load. SNI-carried
   names freeze into consumer configs; arbitrary names would make a
   wildcard-DNS future a breaking migration.
9. **Liveness without machinery.** The token file is re-read per
   connection attempt (`hub/link.rs` precedent). `pair` while running
   works; `pair` on an existing route replaces its token (ADR-0052's
   bijection preserved) — rotation is one command, downtime bounded by
   connector backoff. Deleting a line revokes at the next handshake; live
   tunnels survive until drop or restart (honest; restart is cheap).
10. **Runtime.** `RelayRuntime` with a `run`/`run_async` split mirroring
    `ServerRuntime` ([0003](./0003-server-process-model.md)/
    [0014](./0014-server-terminal-pane-actor.md)); current-thread tokio,
    no `LocalSet`, no daemonize — foreground/systemd. Ctrl-C: immediate
    shutdown, no consumer drain (a reference relay owes no availability
    promise), only a bounded 2s close-frame flush. Stderr banner plus
    enumerated tracing lifecycle lines (tunnel up/down per route,
    per-reason refusals, re-claim warn): diagnostic, non-machine-stable.

## Why

- The trust-honesty claim needs a runnable artifact, not a spike test.
- Every fence in Decision 1 removes a class of infrastructure code.
- Minimal surface is the security argument: the auditable core is the
  SNI gate, the constant-time lookup, and the splice.

## Must-not-preclude invariants for the implementation

1. Fail-closed auth: empty/missing store rejects all; no `--insecure`.
2. Constant-time route lookup: accumulate across all entries, no early
   exit; only a length mismatch may short-circuit.
3. SNI refusal at the TLS layer — the requirement is the invariant, the
   resolver-declines mechanism is not.
4. The relay never parses above TLS; richer dialogue needs an ALPN bump.
5. Stream discipline: one bidi per consumer; stream 0 must transmit.
6. ALPN separation is structural; role never inferred from behavior.
7. 0600 hygiene; cert provisioning no-ops when files exist (fingerprint
   stability is the connector's pinning anchor).
8. Per-connection failure isolation; no peer input kills the endpoint.
9. The crate boundary itself: fewest concepts, not fewest directories.

## Tradeoffs

- A file read per connection attempt: negligible, cheaper than reloads.
- No config file, path flags, or env vars: flags only; revisit if the
  flag count grows.
- A small duplicated `state_dir` beats a `phux-server` dependency edge.
- Live-tunnel revocation needs a restart in v1.
- Last-writer-wins: a stolen token silently displaces the server until
  the operator reads the warn log.
- The relay sees tunnel plaintext — restating ADR-0051's claim, which
  this ADR makes actionable, not fixed.

## Alternatives

- `phux-server` module — invariant-4 hygiene; drags the daemon graph.
- Home in `phux-dial` — its charter is outbound establishment.
- Separate binary — dist surface for no benefit; `[[bin]]` is additive.
- Docs-only status quo — rejected by phux-b1ma.
- Config file — deferrable, therefore deferred.
- Two single-ALPN endpoints — doubles firewall/cert/fingerprint surface.
- `TokenStore` + sidecar route map — two files to desync.
- First-holder-wins — wedged-server lockout (Decision 7).
- Arbitrary route names — forecloses wildcard DNS (Decision 8).
- SIGHUP/hot reload — re-read-per-attempt covers the case that matters.

## Open questions

1. Spec status: `QUIC_RELAY_ALPN`, the tunnel shape, and the close-code
   registry — spec addendum + CHANGELOG (also resolving the pre-existing
   `QUIC_ALPN` drift with phux-zwuz), or ADR-only?
2. Connect-links for relay paths (the link format lacks an sni/route
   param) — fast-follow, not day one.
3. Relay-side consumer admission (defense-in-depth): re-deferred;
   recorded so it is not re-litigated ad hoc — any version needs an
   ALPN bump (invariant 4).

## Related

- Beads: phux-b1ma (this decision), phux-8lyr (epic), phux-tmmb
  (ADR-0052), phux-zwuz (unblocked implementation), phux-qf2w (the
  connector — owns the other end; no server-side claims land here).
- ADRs: [0051](./0051-outbound-dial-out-connector-transport.md);
  ADR-0052 (SNI routing and enrollment binding);
  [0037](./0037-overlay-network-reachability.md) (stance narrowed:
  reference relay in scope, hosted infrastructure still out);
  [0031](./0031-remote-consumer-auth-and-encryption.md) and
  [0038](./0038-hub-satellite-auth.md) (token discipline).
