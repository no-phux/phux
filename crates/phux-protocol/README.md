# phux-protocol

The wire protocol for [phux](https://github.com/no-phux/phux): frame
format, message catalog, capabilities, version negotiation, and the
structured input events a client sends.

phux runs one server per user; clients attach over a local socket or QUIC
and see the same resources. This crate is the contract between them. Each
resource kind declares its stream contract. A Terminal's is asymmetric:
server to client carries the terminal's VT bytes, so a libghostty terminal
on the client reproduces the server's screen exactly; client to server
carries key, mouse, focus, and paste events built from libghostty's
vocabulary, not re-encoded escape sequences. An agent session streams
records, and an accessibility tree, pixel surface, or typed document
streams whole documents.

The code here is the specification. A machine-readable schema of every
frame and field, generated from this codec and test-pinned, is at
[`docs/spec/wire-schema.json`](https://github.com/no-phux/phux/blob/main/docs/spec/wire-schema.json);
[`docs/protocol.md`](https://github.com/no-phux/phux/blob/main/docs/protocol.md)
walks through framing, the handshake, and how changes are made.

## Features

The default build is pure Rust and compiles to wasm: the `wire` codec, the
`input` atoms, `ids`, `caps`, `kinds`, `policy`, `scope`, and `Version`.

The crate has no optional features and never links a VT engine. The
libghostty glue phux's native crates share (render pool, SGR and Kitty
replay, conversions to libghostty's input atoms) lives in the workspace's
unpublished `phux-vt` crate.

## Status

The wire protocol is `1.0`: any 1.x client works with any 1.x server, and
new behavior arrives as feature bits, optional fields, and record tails
([compatibility](../../docs/compatibility.md)). The Rust API is
still `0.x`; pin the crate exactly and read the changelog before upgrading.

Apache-2.0.
