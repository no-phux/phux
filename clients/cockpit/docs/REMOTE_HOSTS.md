---
audience: agents, contributors
stability: evolving
last-reviewed: 2026-09-11
---

# Remote hosts

**TL;DR.** Connect to Host (`cmd+shift+O`, Window > Connect to Host…, or
the switcher's Connect to Host button) points Cockpit's active Phux provider
at a host in the phux CLI's own `[[remote]]` registry, resolved the way
`phux --remote HOST` resolves it. phux-client-ffi's remote tunnel dials QUIC
or TLS WebSocket with the pinned certificate and bearer token, then relays
frames through a Unix-domain socket pair. The socket worker and session
kernel are unchanged. Cockpit holds up to four coordinators at once (this
Mac and registered hosts), each on its own connection, listed as host groups
("This Mac" first). Every terminal identity carries the coordinator that
minted it, so two servers' terminal 7 are two terminals. Picking a session in
another coordinator's group shows it beside the others without disconnecting
anything; a coordinator that fails shows why in its group. Disconnect removes
the hosts. The last host is remembered and reattached beside this Mac on the
next launch.

## Endpoint model

There is one registry: `[[remote]]` in the phux `config.toml`
(`$XDG_CONFIG_HOME/phux/config.toml`, else `~/.config/phux/config.toml`;
[ADR-0055](../../../ADR/0055-machine-registries.md)). `phux host add`,
`phux host enroll` and `phux --remote` write it. Cockpit only reads it,
through phux-client-ffi, which uses phux-config's own loader and schema.

A target is a registry name or `[USER@]HOST[:PORT]`. It is matched as the
CLI's `remote_target::find_entry` matches it: the exact name, then the bare
host, then any entry whose endpoint addresses that host. An explicit `:PORT`
overrides the endpoint for that dial only, and the registry is never
rewritten. `quic://` and `wss://` are dialed, and `ws://` only on loopback.
`ssh://` is refused because it needs a terminal. Trust follows `phux attach`:
a routable host needs its certificate pin, and a routable WebSocket also
needs a bearer token.

Cockpit never pairs a host. Rungs 2 to 4 of the
[resolution ladder](../../../ADR/0093-remote-target-as-a-resolution-ladder.md)
(a pasted code, an ssh pairing, a refusal) write credentials and may prompt,
so they stay in the CLI. An unregistered host fails with a reason naming
`phux --remote NAME` and `phux host enroll NAME`.

A host can also be chosen without the panel:

| Source | Effect |
|---|---|
| `PHUX_REMOTE=NAME` | Dial NAME at launch. Wins over the config. |
| `phux-remote = NAME` in the Cockpit config | Dial NAME at launch. |
| Remembered host (below) | Dial it at launch when neither of the above is set. |
| none | The local coordinator (`phux-socket`, `PHUX_SOCKET`). |

## Data path

```
core.ts --cockpit.remote--> remote_hosts.zig (resolve only; no network)
                              | requestRetarget(endpoint, session, label)
                              v
                            PhuxProvider --- Reconnect's restart path --->
                            Worker.connectRemote
                              socketpair: [0] ordinary framed worker
                                          [1] phux_remote_tunnel_start
                                                resolve -> dial (phux-dial)
                                                -> relay frames
```

- `remote_hosts.zig` resolves the host through
  `phux_remote_tunnel_resolve`, which reads config.toml only, never the
  token file. A failure is answered at once and leaves the current
  connection alone. The tunnel thread reads the token file just before it
  dials and drops its copy once the dial completes.
- A resolved host is set as the provider's pending endpoint. The engine then
  restarts through `restartNavigationConnection`, the path Reconnect uses.
  Frozen canvases, session handoff and command fencing therefore behave as
  they do locally. When the next worker starts, the provider applies the
  pending endpoint, and releases the old coordinator's replicas and session
  identity as an explicit session switch does.
- The worker creates a close-on-exec, `SO_NOSIGPIPE` socket pair. It keeps
  `[0]` for its unchanged length-prefixed I/O and hands `[1]` to the tunnel.
  QUIC carries the same byte stream, so that lane is a byte copy. WebSocket
  carries one frame per binary message, so the tunnel cuts at declared frame
  lengths. Nothing decodes a frame.
- The ABI is documented in `crates/phux-client-ffi/include/phux/client.h`
  under "remote hosts". It is additive to ABI version 2: four functions and
  two versioned structs.

## The `cockpit.remote` request

One bounded request carries the action and returns the resulting status.
It has its own completion slot in the extension bridge, independent of
snapshot and navigation. All lengths are one byte.

| Offset | Request |
|---|---|
| 0 | version 1 |
| 1 | kind: 1 status, 2 connect, 3 use this Mac |
| 2 | target length (non-zero only for connect) |
| 3 | target UTF-8, a `phux-remote` value |

| Offset | Reply |
|---|---|
| 0 | version 1 |
| 1 | phase: 0 local, 1 connecting, 2 connected, 3 failed, 4 reconnecting |
| 2 | host length, then the host (the registry entry's name) |
| then | reason length, then the reason (failed only) |

Host and reason are each elided at 240 bytes on a UTF-8 boundary. The core
asks for status when it opens the panel, when a Connect is waiting, and
when the snapshot's connection byte changes. It never asks once per
snapshot.

## Status and failure reasons

The phase comes from the snapshot's connection state and the provider's
remote record. It is "reconnecting" instead of "connecting" once the
selected host has connected at least once. When a dial or connection fails,
the tunnel publishes the FAILED state and its reason before it closes its
end. The worker copies the reason into the provider-owned record before it
posts the disconnect. The reason therefore survives the worker, and the
status line reads "Could not connect to mini: ...". The host panel keeps
the typed host after a failure, so retrying is one keystroke. It closes
itself once the host it was waiting for connects.

## Persistence and relaunch

Phux-backed layout lives in the coordinator's shared workspace, so the
client only needs to remember which coordinator to reattach to. When a host
chosen through Connect to Host first connects, `<state file>.remote`
records it (`src/cockpit/remote_memory.zig`). A host selected by
`phux-remote` or `PHUX_REMOTE` is never written, so removing the setting
ends it. The file is written the first time a status poll sees the chosen
host connected, so a host that never connected is not remembered.
Disconnect removes the file; Use this Mac leaves it, because the host stays
listed. A torn or foreign file is treated as absent. At launch a
remembered host is reattached beside this Mac, as the standby coordinator
(below), and this Mac is active. A configured or environment host is active,
with this Mac beside it. Saved placements carry the endpoint
`phux-remote:<target>`, which is disjoint from every absolute socket path,
so a placement never matches the wrong coordinator.

## Several coordinators

The model holds the active Phux provider (`Model.phux_provider`) and up to
three more (`Model.phux_peers`): this Mac's coordinator while a remote host
is active, and registered hosts. Each peer runs its own socket worker on its
own channel (`phuxPeerChannelKey(slot)`: 104, 105, 106), so any one restarts
or fails alone.

### Identity

A terminal's identity is the coordinator that minted it plus the server's
id: `TerminalRef.provider_id` is `contract.phuxCoordinatorId(target)`. This
Mac's coordinator is `.phux`, the id every saved placement already carries;
a registered host's is derived from its registry target, with bit 31 set so
it can equal neither `.phux` nor `.local`. The id follows the endpoint, so it
is stable across reconnects and relaunches and moves with a retarget.

Every provider mints refs with its own id (`Host.provider_id`: replicas,
workspace nodes, catalog rows, agent sessions, operation results, bells),
and refuses a ref with any other id: another coordinator's terminal 7 is
never looked up, resized, typed into or detached here. The model routes each
ref to the provider that minted it (`Model.phuxForRef`), which is how input,
sizing, painting, selection, search, bells, titles and catalog targets all
reach the right machine. A catalog target is captured against its
coordinator's own context, so a target for a coordinator no longer held, or
retargeted since, resolves to nothing. Saved attachment evidence is kept for
the active coordinator's refs only; its endpoint and provider id are both
persisted.

### Listing and showing

A peer starts by LISTING. It never attaches: an attached client is a
subscriber, and under the server's default `window-size = smallest` its
viewport would size every pane of its session for everyone else, and every
pane's output would stream to it for nothing. After HELLO_OK it asks GET_STATE
(`phux_client_query_sessions`), once per connection and again whenever the
switcher refreshes. Its group lists only while that list belongs to the
current connection and no retarget is pending; on disconnect and the moment
it is retargeted it forgets the list and its generation, so rows captured
from it stop resolving.

Picking one of its sessions makes it SHOW that session
(`Engine.showPeerSession`): only that peer restarts its connection, and its
first frame after HELLO_OK is ATTACH for that session, by id. Its shared
workspace then projects into the same windows as the active coordinator's
(`Model.peer_workspaces`): each coordinator's publication replaces only its
own tabs, keeps every other showing coordinator's tabs in place with their
selection, and puts its own group back where it was. Each of its terminals
is sized on its own server by the sizing pump once a pane shows it. Picking
another session of a showing peer leaves the first session's tabs for it.

A peer stays shown only while one of its tabs is the selected, painted tab
of an open window. Its first projection takes the selection. Once none of
its tabs is on screen (another tab or session was chosen, its tabs were
closed, or its session has no windows) it returns to listing
(`Engine.settlePeers`, after every native mutation and channel wake): its
tabs leave and only its connection restarts as a standby, so the attach and
every viewport it held end and the new connection asks GET_STATE. Picking
one of its sessions shows it again. Close Tab and Close Pane on a peer's
tab go to that coordinator as the same layout-only removal the active
coordinator's tabs send to theirs; its terminals keep running there. Go to
Directory over a peer's pane lists through that peer (see
[Go to Directory](DIRECTORY_PICKER.md#panes-of-another-coordinator)).

| Action | Active | Peers |
|---|---|---|
| a session in a peer's group | unchanged | that peer shows it, beside the others |
| Connect to Host | the host | the coordinator it left joins the list; a listed host trades places |
| Use this Mac | this Mac | the host joins the list, others unchanged |
| Disconnect | this Mac | none: every host's group and tabs go |

Connect to Host and Use this Mac retarget two providers through
`requestRetarget` and restart them through the path Reconnect uses; no other
coordinator moves. A peer that trades places first takes its tabs out of the
windows and stands by, because its identity is about to change. The switcher
lists this Mac's group first, then registered hosts.

### A failed coordinator

A peer whose connection fails or closes, or whose channel cannot open, is
marked failed until it lists again. Its group then shows one row, named for
the host, reading "Unavailable" with the recorded reason (a remote host's
dial or connection failure), else "the connection was lost". Picking that
row dials that peer again (`Engine.retryPeer`); the row then reads
"Connecting…" and is not selectable until the peer fails again or lists. A
showing peer's tabs keep their last frames, frozen, while it is down. If its
server ends the shown session instead, its tabs leave and it goes back to
listing on a fresh connection. A showing peer whose workspace cannot be
projected reads "workspace unavailable" on its session rows.

## Known limits

- At most four coordinators: the active one and three peers. Connecting to a
  fifth is refused with a reason.
- Disconnect removes every registered host, not one of several. Only the
  most recently connected host is remembered for relaunch.
- New tabs, splits and the available-terminal inventory belong to the active
  coordinator. A showing peer's panes are typed into, selected, searched and
  sized on their own host, and its tabs and panes close there. Split,
  reorder and split-drag on a peer's tab are refused (a split-drag snaps
  back), without marking the active coordinator's workspace refused. New Tab
  with a peer's pane focused opens on the active coordinator. Go to
  Directory over a peer's pane lists that peer's host, but cannot open a tab
  there.
- A peer is shown only while one of its tabs is on screen. Keeping a peer's
  tab in the background while another coordinator's tab is selected is not
  possible: choosing that other tab returns the peer to listing.
- A showing peer's placements are not saved as attachment evidence; they
  are projected again from its workspace when it reconnects. A failed peer
  is not restarted automatically: picking its group's row retries it.
- A channel close event carries only its slot's key. One that arrives after
  Disconnect and a new Connect reused the slot is taken for the new peer's:
  it stops that peer's connection and marks it failed. Picking its row
  redials it.
- Showing a peer session is refused while the active coordinator has a
  retarget pending, since the two briefly share a coordinator id.
- Placements saved for a remote active host before coordinator ids existed
  carry `.phux` and are not matched; that host's workspace projects them
  again.
- Known-host rows group terminals by satellite name; two coordinators' own
  terminals share the coordinator row.
- Catalog search matches titles, directories, sessions and peer host labels.
- A registry entry's pinned `session` is requested both by Connect to Host
  and when a host selected at launch (remembered, `phux-remote` or
  `PHUX_REMOTE`) is attached. An explicit `phux-session` or `PHUX_SESSION`
  wins over the pin, as a session named on `phux --remote` does. Only when
  neither is set does the remote server's own last-attach memory decide.

## Validation

```sh
cargo nextest run -p phux-client-ffi -E 'test(/remote/)'
just cockpit-test
cd clients/cockpit
node --import ./src/tests/navigation-loader.mjs --test ./src/tests/remote-hosts.test.mjs
```

The FFI tests run a real loopback WebSocket peer through the tunnel. They
also prove that a failure reason is readable before EOF, and that `free`
cancels an unanswered dial. The worker tests in `extension.zig` prove the
same ordering one layer up. None of these is live macOS or remote-network
acceptance.
