---
audience: contributors
stability: stable
last-reviewed: 2026-10-06
---

# 0150 — The server keeps the restore archive current

**TL;DR.** `phux service install --restore` runs `phux server --autosave
PATH`. The server restores `PATH` once on a fresh start, then rewrites it
atomically (temp, fsync, rename) a few seconds after the workspace changes
and at least once a minute, so a crash, `SIGKILL`, abort, or power loss
restores the latest layout. The wrapper keeps its save on stop.

Status: Accepted
Date: 2026-10-06

## Context

ADR-0055 wired `--restore` as a shell wrapper that saved the archive on
`TERM` and restored it once the socket appeared. Only a clean stop ever
saved. A crash, `SIGKILL`, a `panic = "abort"`, or power loss left no
archive, or one from the last clean stop days earlier, so the supervised
restart brought back a stale layout or none. The save itself was
`workspace save` to a temp file and `mv`, with no fsync, so even a clean
save could be lost to power loss.

## Decision

1. **The server owns the archive's freshness.** `phux server --autosave
   PATH` arms an autosave. Once a second the server thread hashes what the
   archive restores structurally (sessions, names, keep-empty marks, window
   order and focus, pane placement and cwd, live agent-session identities
   (ADR-0151), and changes to layout envelopes and native agent-session
   records (ADR-0068)); the hash is in memory and
   does no I/O. A dedicated thread saves a change once it has been quiet for
   2 s (at most 10 s behind a workspace that never settles), and at least
   once a minute for drift the hash leaves out (titles, sizes). An unchanged
   capture is not rewritten.
2. **What an archive is stays one composition.** The saver dials the
   server's own socket and runs the same capture as `phux workspace save`,
   passed in by the binary; `phux-server` never learns the archive schema.
3. **Every archive write is atomic and durable.** Temp file (`0600`),
   fsync, rename, directory fsync, for the autosave and for `workspace save
   --output` alike.
4. **Restore moves into the server.** With `--autosave` the server restores
   `PATH` itself on a cold start, before the autosave arms, so no save can
   race the restore that reads the file. A hot upgrade (ADR-0032) re-emits
   `--autosave` and does not restore again. The wrapper no longer restores;
   it keeps the final save on stop.
5. **Shutdown never saves a torn workspace.** Every shutdown cancels the
   root token before tearing down a pane; a capture finishing after that is
   discarded.
6. **A failed restore is not overwritten unprompted.** The archive is copied
   to `PATH.unrestored`, and only a later workspace change saves over it.
7. **A dev build refuses a production path** at startup, through
   `refuse_dev_on_production_state`.

## Why

- The server is the only process that knows when the workspace changed. A
  wrapper or plugin loop would poll by forking `phux workspace save`, paying
  a process and several round trips per tick to learn that nothing moved.
- Hashing in memory beats instrumenting every mutation site: the registry is
  mutated from many paths, and a missed site would be a silent stale
  archive. The floor catches anything the hash leaves out.
- Reusing the client capture keeps one definition of the archive; a
  server-side serializer would drift from `workspace save` and `restore`.
- Ordering restore before autosave inside one process removes the race a
  wrapper restore would have with the first autosave.

## Tradeoffs

- A change made within about three seconds of a crash is lost. The save on
  a clean stop still covers the last moments of an orderly shutdown.
- Titles, pane sizes, and the focused session reach the archive up to a
  minute late.
- The saver is a client of its own server: a capture costs a few local
  round trips, bounded by the debounce.
- Units installed before this keep the old wrapper until `phux service
  install --restore` is rerun.

## Alternatives

- **Periodic `workspace save` in the wrapper or the continuum plugin.**
  Forks a process per tick and cannot tell a change from no change.
- **Server-side archive serializer.** Duplicates the capture and the agent
  record reconciliation the binary already owns.
- **A journal of mutations replayed on restart.** ADR-0130 declined durable
  server journals; the archive needs a snapshot, not a log.
