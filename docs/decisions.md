# Decisions

These are current constraints, not a claim that every existing implementation
is correct. A change that conflicts with one needs an explicit decision and
updated tests and guidance—not a quiet rewrite that blesses a bug. A lasting
trade-off belongs here with its reason and the PR that made it. Older numbered
records explain history; [look them up deliberately](./architecture.md#historical-decisions).

**Process and runtime**

- One server per user per profile; one current-thread runtime; a pane is a
  `LocalSet` task that owns its `!Send` libghostty terminal.
- Hot upgrades re-exec and re-adopt live PTYs; cold restarts, crashes and
  reboots do not preserve PTYs. Records are not process recovery.
- The upgrade blob's field names are a format. A candidate binary must read
  the staged blob before the running image execs into it, and every shape a
  release wrote stays frozen in an append-only fixture, because a resume
  failure kills every pane.
- Exit is a facet, close is a purge. Remove policy removes on exit; Retain
  keeps an inspectable result until purge/expiry. [Organization](./organization.md)
  owns commit and reconciliation rules.
- Events are journaled and resumable; a reconnecting watcher gets what it
  missed or a gap marker, never silence.

**Wire**

- Server to client is VT bytes; client to server is structured input. Both
  sides run libghostty.
- Any 1.x client works with any 1.x server, older or newer. New behavior
  ships as a feature bit or a skippable field or record tail; a server
  answers what it does not know with `UNSUPPORTED` and stays connected. A
  new major is a deliberate, announced break
  ([compatibility](./compatibility.md)).
- The kind catalog in `phux-protocol::kinds` is the one table for discovery
  and authorization; anything it does not name is denied.
- `wire-schema.json` is generated from the codec and test-pinned; the code is
  the specification.
- `phux-core` and `phux-protocol` stay independent; the server bridges them.
- File transfer rides the command envelope, never PTY input or a second
  transport, and addresses a machine by host name, not a pane. `PUT_FILE`
  writes only into a server-chosen sandbox because a client-named write is
  an execution surface; `GET_FILE` reads any file the serving user can read
  because an `INPUT` holder on that server can already `cat` it, so the read
  is classified `INPUT` on the server rather than fenced by a path allow-list
  ([protocol](./protocol-files.md#file-transfer),
  [#1657](https://github.com/no-phux/phux/pull/1657)).
- Checkout methods are typed requests keyed by a Terminal, not a generic
  exec or a string-named invoke. The server resolves the checkout from the
  Terminal's live working directory and runs the shared `phux-git` module,
  so mobile, remote desktop, TUI and MCP read one contract; a client never
  names a host path. They are `INPUT` on that one Terminal, not on the
  server as `GET_FILE` is: an `INPUT` holder on it can already run `git`
  there. A Terminal whose working directory is inside a large checkout (say,
  dotfiles tracked at `$HOME`) therefore exposes every file in that
  checkout outside `.git` to that Terminal's `INPUT` holders
  ([protocol](./protocol-files.md#checkout-methods)).
- A host service (OpenCode) is reached through the phux connection, not its
  own port, tunnel, or pairing broker: the server names the service, owns
  its loopback origin and mints its own credential, and relays one HTTP
  exchange at a time that the client pulls in chunks. phux authorization is
  the only gate, `INPUT` on the server as for `GET_FILE`, so revoking a
  device revokes its OpenCode access and no service credential reaches it
  ([host services](./host-services.md)).

**Public operations**

- [Terminal creation](./public-api.md) expresses a complete intention: destination,
  direct argv and placement. Membership, arrangement entry and launch intent commit
  together; OS execution follows, not a fictitious database/process transaction.
  New API resources retain failed/completed execution records for reconciliation.
- Keep the binary transport and shared runtime. Public JSON DTOs and nested
  schemas describe the workflow, not transport tags or metadata carriers. The
  connected server serves the description; installed-binary discovery remains
  separate. No generic invocation framework or second listener is introduced.
- A retry identity has a bounded retained receipt, not an exactly-once promise
  forever. A read-only creation lookup can recover lost replies without effects.
  Missing/expired receipts leave the original outcome unknown; never infer that
  it is safe to start again. Dispatch-time authorization remains authoritative.

**Clients**

- `phux-client-core` is sans-IO and dependency-light so it compiles to wasm.
  `phux-client-runtime` is the one place client orchestration lives.
  `phux-client-ffi` is the one binding crate.
- Pane interiors are libghostty's; chrome is the client's. The two never
  paint the same cell.

**Agents and authority**

- The server derives agent state. A host's lifecycle stream outranks the
  detector; an occupant-bounded declaration outranks both.
- The ordinary owner socket has full same-UID authority; delegated sockets
  only narrow it. Remote consumer ingress requires scoped mTLS plus bearer
  admission; reverse federation grants the trusted hub satellite-owner
  authority. Bearer-only and anonymous remote ingress are gone.
- Clients pin the server's certificate authority at pairing and never learn
  one from a connection. Devices and satellites enroll with single-use
  tickets.
- Humans outrank agents on input leases. Scoped grants can be held for
  approval; the owner never is.
- Plan usage is a plugin publishing a Global L3 key (`phux.usage/v1`), not a
  wire verb or server code: last-writer-wins host facts belong in metadata,
  and clients watch it with the generic `SUBSCRIBE_METADATA`. Declared
  producers and feeds (ADR-0177) remain the upgrade path if a fact outgrows
  one key ([agents](./agents.md#plan-usage)).

**Substrate**

- One keeper writes each work store; shared work has one authoritative home
  store. Adapters are one-way to avoid competing task authorities. Blackbird
  is retired from phux coordination. Reverse join grants satellite-owner
  authority and is only for one person's own machines; delegated credentials
  cannot mint, and cold restart requires human reauthorization.
- The accepted future coordinator design uses a separate
  `phux-coordinator/1` endpoint with an external fencing witness, never the
  terminal stream. That coordinator, durable grant ledger, autonomous Lead,
  per-agent cross-host grants, and organization-to-organization trust do not
  ship today
  ([substrate](./substrate.md),
  [#1689](https://github.com/no-phux/phux/pull/1689)).

**Not doing**

- No embedded scripting language: commands are typed messages; logic lives
  in scripts that call `phux`.
- No in-process plugin host: plugins are external processes declared in
  config.
- No homegrown selection or search engine beyond find-in-scrollback:
  selection and extraction belong to libghostty.
- No homegrown crypto: TLS, SSH, and Unix permissions.
- No tmux behavior "for compatibility" where phux has a reason to differ.

**Release**

- Release Please versions from conventional commits. Checksums are minisign
  signed and the installer verifies them. One manifest per channel.
- Generated reference pages are byte-pinned by a test; hand edits fail CI.
