---
group: internals
order: 1
---

# Decisions

The constraints the code keeps, each with the reason a contributor would
otherwise break it. Changing one takes an explicit decision with updated tests
and docs, not a quiet rewrite. Retired records are
[archived](./architecture.md#historical-decisions).

## Process and runtime

- One server per user per profile on one current-thread runtime; a Terminal
  is a `LocalSet` task owning its `!Send` libghostty terminal.
- Hot upgrades re-exec and re-adopt live PTYs. Cold restarts, crashes and
  reboots do not preserve PTYs, and records are not process recovery.
- The upgrade blob is a format. The candidate binary must parse the staged
  blob before the running server execs into it, and every released shape stays
  in an append-only frozen fixture, because a failed resume kills every pane.
- Exit is a facet; close is a purge. `Remove` drops a resource on exit,
  `Retain` keeps the result until purge or expiry
  ([organization](./organization.md)).
- Events are journaled and resumable: a reconnecting watcher gets what it
  missed or a gap marker, never silence.
- A detached process group is signalled only while its recorded leader
  still matches, never by a reused PID.
- An upgrade rebuilds resources in `APP_KINDS` order, so a parent's kind
  comes first.
- A session `ATTACH` never replaces committed `AgentSession` history with an
  unsolicited full cut.
- A close is planned before it commits. Tab-lock checks run on the plan,
  including bound descendants, under the same state lock; children close
  before their parents ([close intents](./organization.md#close-intents)).

## Wire

- Each kind declares its stream contract and spawn rule in its `KindSpec`
  catalog row. A connection that negotiates no Terminal profile is still
  admitted; binding a Terminal stream is refused with `CODEC_UNAVAILABLE`.
- Stream and input frames reach their resource's `ResourceApp`. A kind that
  does not serve a frame refuses it with `WRONG_RESOURCE_KIND`, and the
  connection stays.
- Any 1.x client works with any 1.x server. New behavior ships as a feature
  bit or a skippable field; unknown requests get `UNSUPPORTED` and the
  connection stays ([compatibility](./compatibility.md)).
- The kind catalog in `phux-protocol::kinds` is the one table for discovery
  and authorization; anything it does not name is denied.
- `docs/spec/wire-schema.json` is generated from the codec; the code is the
  specification.
- `phux-core` and `phux-protocol` stay independent; the server bridges them.
- File transfer rides the command envelope, never PTY input. `PUT_FILE` writes
  only into a server-chosen sandbox, because a client-named write is an
  execution surface. `GET_FILE` is classified `INPUT` on the server, because
  an input holder can already `cat` the file ([files](./protocol-files.md)).
- Checkout methods are typed requests keyed by a Terminal and resolved from
  its working directory; they need `INPUT` on that Terminal, which can
  already run `git` there. A client never names a host path.
- Host services and TCP forwarding go through the phux connection, never a
  new port or stream type, and need `INPUT` on the server. Forwarding dials
  only loopback plus `[forward] allow` targets and never resolves a client's
  name ([ssh](./ssh.md)).
- ssh-agent forwarding brokers only key listings and signatures, so nothing
  can add, remove or lock keys. Both sides opt in (`[forward] agent`,
  `phux forward --agent`), and the client asks before each signature unless
  run with `--yes`.
- Slot tabs are trailing fields on the leaf record, not a new node kind,
  because a new kind would break older clients' arrangement reads and older
  servers' store loads ([slot tabs](./organization.md#slot-tabs)).

## Public operations

- [Terminal creation](./scripting.md) commits membership, arrangement entry
  and launch intent together; OS execution follows and can fail separately.
- A retry identity has a bounded retained receipt, not an exactly-once
  promise. A missing or expired receipt leaves the outcome unknown; never
  infer that it is safe to start again.
- Public JSON describes workflows, not transport tags. There is no generic
  invoke framework and no second listener.

## Clients

- `phux-client-core` is sans-IO and compiles to wasm. `phux-client-runtime`
  is where client orchestration lives. `phux-client-ffi` is the one binding
  crate.
- Pane interiors are libghostty's; chrome is the client's. They never paint
  the same cell.

## Agents and authority

- The server derives agent state. A host lifecycle stream outranks the
  detector; an occupant-bounded declaration outranks both.
- The server names agent panes after their kind, unique on the server
  (`opencode`, `opencode-2`), and a name never moves to another pane. A
  chosen name is stored as written; a shared one is ambiguous, never guessed.
- The owner socket has full same-UID authority; delegated sockets only narrow
  it. Remote consumers need scoped mTLS plus bearer admission
  ([remote](./remote.md)).
- Clients pin the server's CA at pairing and never learn one from a
  connection. Enrollment tickets are single-use.
- An organization-managed workload credential is admitted by the host's
  registry entry; the CA only lets the certificate verify.
- Humans outrank agents on input leases. Scoped grants can be held for
  approval; the owner never is.
- Plan usage is a plugin writing the Global metadata key `phux.usage/v1`, not
  a wire verb: last-writer-wins host facts belong in metadata.
- A program can hand phux a resume token (OSC 7501 `resume=`) but never a
  command. A relaunch runs only the argv in `[[restore.program]]`, with the
  token as whole arguments and no shell, because any pane writer can set it
  ([program protocol](./program-protocol.md)).
- One keeper writes each work store, and shared work has one home store.
  Delegated credentials cannot mint ([substrate](./substrate.md)).

## Not doing

- No embedded scripting language: logic lives in scripts that call `phux`.
- No in-process plugin host: plugins are external processes.
- No homegrown selection or search beyond find-in-scrollback; that belongs to
  libghostty.
- No homegrown crypto: TLS, SSH and Unix permissions.
- No second peer-to-peer transport. Direct QUIC/WebSocket, ssh and the
  ciphertext-only relay already avoid VPNs and open ports; another would add a
  second trust model to every client.
- No tmux behavior "for compatibility" where phux has a reason to differ.

## Release

- Release Please versions from conventional commits. Checksums are minisign
  signed and the installer verifies them.
- Generated reference pages are byte-pinned by a test; hand edits fail CI.
