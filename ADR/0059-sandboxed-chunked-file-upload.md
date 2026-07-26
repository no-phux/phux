---
audience: contributors
stability: stable
last-reviewed: 2026-07-25
---

# 0059 — Sandboxed chunked file upload

**TL;DR.** Add one capability-negotiated `PUT_FILE` command under the existing
command envelope. A consumer sends bounded, offset-addressed chunks under a
random upload id; the terminal-owning server writes only inside its own upload
directory, acknowledges every retained offset, verifies whole-file SHA-256, and
returns the completed absolute path. Clients never choose server paths.

Status: Accepted
Date: 2026-07-25

## Context

A projection consumer sometimes needs to hand a local artifact to a program in
a remote terminal. phux-mobile's first case is an image selected on iPhone that
Claude Code can consume by filesystem path. The wire carried terminal input but
no file transfer.

ADR-0020 in phux-mobile falsified the tempting workaround: base64 through a
hidden shell and `INPUT_PASTE` corrupted even small payloads nondeterministically
and canonical-mode PTY ingest was minutes-per-photo slow. Shell quoting,
heredocs, and a digest gate cannot turn the PTY input path into a file transport.
The server must own the write.

## Decision

1. Add `PUT_FILE` as L1 `Command` tag `0x15` under the existing `COMMAND` /
   `COMMAND_RESULT` envelope. Bump the protocol version from `0.5.0` to `0.6.0`
   and advertise `ServerFeature::FileUpload` at bit `0x00000020`. A client MUST
   negotiate the feature before sending the command.
2. Each chunk carries a non-zero 128-bit consumer-generated upload id, a
   `TerminalId`, a short ASCII filename extension, a byte offset, raw bytes, a
   final-chunk flag, and a whole-file SHA-256 digest only on the final chunk.
   Chunks are at most 8 MiB; a completed upload is at most 64 MiB.
3. `TerminalId` identifies the destination host and requires a live Terminal on
   that host. It does not choose a directory or grant a client path. A federation
   hub rewrites a satellite Terminal id and relays the command; the satellite
   owns storage, digest verification, and the returned path.
4. The terminal-owning server maps the upload id to one partial file under
   `$PHUX_UPLOAD_DIR`, `$XDG_DATA_HOME/phux/uploads`, or
   `$HOME/.local/share/phux/uploads` in that order. The directory is mode 0700;
   files are mode 0600. The final name is server-generated as
   `phux-upload-<id>.<extension>`. Separators, dots, Unicode, and extensions over
   16 bytes are rejected, so traversal is structurally impossible.
5. Chunks are offset-checked. A retry whose retained overlap is byte-identical
   advances to the same next offset without duplicating bytes; a gap or
   same-id/different-byte reuse is `INVALID_COMMAND`. Partial state is durable
   across reconnects and server restarts because it is the file itself, not an
   in-memory ledger.
6. Every accepted chunk replies
   `OK_WITH(FILE_UPLOAD { next_offset, path? })`. `path` is absent until a final
   chunk's SHA-256 matches the complete partial file. The server syncs and
   atomically renames the partial file before returning the path. Digest mismatch
   leaves the partial file unexposed for a corrected retry.
7. Completed files remain in the upload directory until the user or a future
   explicit retention policy removes them. Version one does not delete user
   artifacts behind a running agent. Stale-part cleanup is deferred; partials
   are hidden and bounded per upload, and support tooling can identify them.
8. Payloads and completed paths MUST NOT enter routine telemetry. The command
   lifecycle label is only `put_file`; errors describe the class without logging
   file bytes.

## Why

- The command envelope already provides request correlation, typed failure, and
  relay reply mapping. A parallel upload frame family would duplicate it.
- A server-chosen path removes traversal and avoids treating client path syntax
  as authority.
- Offset acknowledgements make reconnect continuation explicit. Whole-file
  digest verification catches storage, relay, and client chunking faults before
  a path is handed to an agent.
- A Terminal target gives federation a routing key without introducing a second
  host-identity namespace or exposing arbitrary remote filesystem writes.
- Raw bytes avoid the 4/3 base64 expansion and PTY/canonical-mode behavior.

## Tradeoffs

- The v0 command uploads one file serially and retains a process-wide critical
  section during local disk writes. With an 8 MiB chunk ceiling this is bounded,
  simple, and avoids concurrent offset races; a high-throughput bulk-transfer
  lane can replace it if measurements justify the complexity.
- A live Terminal is required even though the write does not touch its PTY. This
  deliberately narrows authority and supplies federation routing, but prevents
  uploading to a host with no panes.
- Completed files consume disk until explicitly removed. Silent time-based
  deletion would race agents reading a path, so retention needs its own visible
  product contract.
- The returned absolute path reveals the server account's upload-root prefix to
  the authenticated consumer. That consumer already controls a terminal as the
  same per-user server; the path is not logged or shared with other peers.

## Alternatives

- **Base64 through terminal input.** Rejected by live evidence: corrupt, slow,
  shell-dependent, and capable of killing the pane input writer.
- **Let the client provide a destination path.** Rejected: canonicalization and
  symlink policy become a remote arbitrary-write surface.
- **One 16 MiB frame per file.** Rejected: framing overhead leaves less than the
  nominal cap, photos can exceed it, and reconnect cannot resume.
- **HTTP upload endpoint.** Rejected: it creates a second authenticated
  transport, capability handshake, federation route, and error model for one
  operation the command plane already supports.
- **Store bytes in L3 metadata.** Rejected: metadata is durable coordination
  state, not a bulk-object store; it would amplify snapshots and subscriptions.

## Related

- ADR-0007 — federation hub and satellite routing.
- ADR-0021 — control-plane command envelope.
- ADR-0031 — remote consumer authentication and encryption.
- ADR-0053 — acknowledged idempotent input batches.
- phux-mobile ADR-0020 — image paste needs a wire verb.
- Mobile bead: phux-q7e.18.
- PTY corruption follow-up: phux-6b6t.
