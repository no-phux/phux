# phux-protocol

The wire protocol for [phux](https://github.com/no-phux/phux) — a terminal
control plane where a human and an agent share the same live terminal.

phux runs one server per user; clients attach over a local socket and see the
same panes, the same scrollback, the same cursor. This crate is the contract
between them: the frame format, the message catalog, version negotiation, and
the shapes that carry terminal content and structured input across the wire.

It is the source of truth. The narrative spec lives in
[`docs/spec/`](https://github.com/no-phux/phux/tree/main/docs/spec); this crate
is the normative encoding of it, and everything else in the workspace defers
to it.

## What's on the wire

The wire is deliberately asymmetric (see
[ADR-0013](https://github.com/no-phux/phux/blob/main/docs/adr/0013-libghostty-bytes-on-wire.md)):

- **server → client** carries **VT bytes** forwarded from the PTY — the
  terminal's own output, unmodified, so a libghostty terminal on the client
  reproduces the server's screen exactly.
- **client → server** carries **structured input** — key, mouse, focus, and
  paste events built from libghostty's own atoms
  ([ADR-0008](https://github.com/no-phux/phux/blob/main/docs/adr/0008-use-libghostty-types-directly.md)),
  not re-encoded escape sequences.

The protocol is layered: an L1 terminal substrate, an L2 collection layer, and
an L3 metadata layer
([ADR-0015](https://github.com/no-phux/phux/blob/main/docs/adr/0015-protocol-layering.md)).

## Features

This crate publishes to crates.io as a near-empty shell by default and grows
its full surface behind a feature flag. The full surface depends on
`libghostty-vt`, resolved through the workspace dependency in this repository
and through crates.io for published consumers:

- **default** — stable IDs (`ids`), capability atoms (`caps`), and the
  protocol-version constant.
- **`server`** — the full type surface: the `input` and `wire` modules and
  the libghostty input-atom re-exports. Every in-workspace consumer enables
  it; an external consumer that vendors libghostty will too.

```toml
[dependencies]
phux-protocol = "0.0"            # IDs + caps + version
phux-protocol = { version = "0.0", features = ["server"] }  # full wire surface
```

## Status

Early and moving. The version is `0.x`; the wire is versioned and the spec
carries a changelog, but the shapes here are still settling as the rest of
phux lands. Pin exactly and read the
[spec changelog](https://github.com/no-phux/phux/blob/main/docs/spec/CHANGELOG.md)
before you upgrade.

## License

Apache-2.0.
