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
the host named in the panel, and Disconnect All every host. The hosts
connected through Connect to Host are remembered and come back beside this
Mac on the next launch, listing until one of their sessions is shown.

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
| 1 | kind: 1 status, 2 connect, 3 use this Mac, 4 disconnect |
| 2 | target length: required for connect; for disconnect, empty means every host |
| 3 | target UTF-8, a `phux-remote` value |

| Offset | Reply |
|---|---|
| 0 | version 1 |
| 1 | phase: 0 local, 1 connecting, 2 connected, 3 failed, 4 reconnecting, 5 refused |
| 2 | host length, then the host (the registry entry's name) |
| then | reason length, then the reason (failed and refused only) |

Refused means the request changed no connection: a Disconnect naming a
host Cockpit does not hold (a remembered one is still forgotten, and the
reason says so), a name matching more than one held host, or a Connect with
no room for another coordinator.
The panel shows the reason and keeps the connection status as it was.

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

Phux-backed layout lives in each coordinator's shared workspace, so the
client only needs to remember which coordinators to reattach to. When a host
chosen through Connect to Host first connects, it joins the hosts
`<state file>.remote` records (`src/cockpit/remote_memory.zig`): up to three,
oldest first, one `target=` line each. A file an earlier release wrote, with
one host, still reads. A host selected by `phux-remote` or `PHUX_REMOTE` is
never written, so removing the setting ends it. A host is added the first
time a status poll sees it connected, so a host that never connected is not
remembered. Disconnect removes the host it names from the file, and
Disconnect All removes the file; Use this Mac leaves it, because the hosts
stay listed. A torn or foreign file is treated as absent.

At launch the remembered hosts are reattached beside the coordinators held,
each LISTING in its own slot (`startup.attachRememberedPeers`), as many as
there are free slots; none attaches until one of its sessions is shown. With
no host configured this Mac is active; a configured or environment host is
active with this Mac beside it (so two remembered hosts fit), and is not held
a second time if it is also remembered. A host that cannot be set up is
skipped, not the launch, and stays remembered. Disconnect forgets only what
Connect to Host remembered: a host named by `phux-remote` or `PHUX_REMOTE`
is active again on the next launch while the setting stands.

A Phux-backed launch keeps no client-side layout for any coordinator: the
state file is not read, and saved attachment evidence is discarded when the
Phux provider is attached. A coordinator's tabs come back from its own
shared workspace when it connects (a peer's, when one of its sessions is
shown). So placements saved before coordinator ids existed, which carry
`.phux` beside a remote host's `phux-remote:<target>` endpoint, never match
anything; that host's workspace projects its terminals again under its own
coordinator id. Within a run, saved placements carry the endpoint
`phux-remote:<target>`, which is disjoint from every absolute socket path,
so a placement never matches the wrong coordinator.

## Several coordinators

The model holds the active Phux provider (`Model.phux_provider`) and up to
three more (`Model.phux_peers`): this Mac's coordinator while a remote host
is active, and registered hosts. Each peer runs its own socket worker on its
own channel, so any one restarts or fails alone. A slot's first channel is
`phuxPeerChannelKey(slot)` (104, 105, 106). Every close Cockpit asks for (a
restart, a failure, a Disconnect) moves the slot to its next generation, and
the next channel opens under a key carrying slot and generation
(`phuxPeerChannelKeyAt`). An event from a closed occupancy is recognized by
its key and ignored, even when the slot has since gone to another peer; only
the close a restart waits for acts, by opening the next channel.

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
one of its sessions shows it again.

Every edit of a peer's tab goes to that coordinator and to no other
(`src/cockpit/peer_edits.zig`): Close Tab and Close Pane (the same
layout-only removal the active coordinator's tabs send to theirs; its
terminals keep running there), splits, reordering within its own order, and
a divider drag. New Tab with one of its panes focused opens on it, and so
does Open a new tab here on a directory listing made through it (see
[Go to Directory](DIRECTORY_PICKER.md#panes-of-another-coordinator)). Each
peer has its own edit queue, with the checks the active coordinator's queue
makes: connection, session, revision, target, and one edit at a time. A new
tab or split spawns on the peer first and is placed once its terminal
publishes live, and the confirmed placement selects it unless the user has
chosen something else meanwhile. A peer that is not showing a projected
session takes no edit, and the edit is refused rather than sent anywhere
else. A refused peer edit names that peer's workspace on its switcher rows,
never the active coordinator's. A restart, failure or removal of the peer
forgets its queued edits; one already sent may still land on that server.

| Action | Active | Peers |
|---|---|---|
| a session in a peer's group | unchanged | that peer shows it, beside the others |
| Connect to Host | the host | the coordinator it left joins the list; a listed host trades places |
| Use this Mac | this Mac | the host joins the list, others unchanged |
| Disconnect (a named host) | this Mac if that host was active, else unchanged | that host's group and tabs go; the others stay |
| Disconnect All | this Mac | none: every host's group and tabs go |

Disconnect takes the host typed in the panel. An exact target is matched
first, across the active host and every peer; only when no target matches
is a registry name tried. The same registry entry can be held twice under
two typed targets (`mini` and `me@mini` both name `mini`), so a name that
matches more than one held host is refused with every target it matched,
and nothing is removed. Removing the active host hands it over to this
Mac, whose standby slot is freed rather than listed twice. A host Cockpit
does not hold but still remembers (one that found no free slot at launch)
is forgotten, by target or by its registry name when that names one
remembered host, and the panel says it was not connected.

Connect to Host and Use this Mac retarget two providers through
`requestRetarget` and restart them through the path Reconnect uses; no other
coordinator moves. A peer that trades places first takes its tabs out of the
windows and stands by, because its identity is about to change. The switcher
lists this Mac's group first, then registered hosts.

### Renaming a session

Window > Rename Session… renames the session on screen: the session whose
tab holds the focused pane, on the coordinator that minted that pane's ref
(`Engine.renameTarget`). With no Phux pane focused it is the active
coordinator's attached session. A pane whose coordinator Cockpit no longer
holds names nothing, and the rename is refused rather than sent to another
coordinator; a listing peer's sessions are never on screen, so a rename never
reaches one. The write is `phux.session.name/v1` (`current\0new`,
[L3.md](../../../docs/spec/L3.md) section 3.1) on that coordinator's own
connection, through `phux_client_rename_session`. It writes metadata only:
nothing attaches and no viewport changes.

The name is judged first against that coordinator's list as Cockpit shows
it, then by phux-client-ffi against the client's own list, which subscribes to
the key and confirms the write with a `GET_STATE` barrier. A name another
session holds, a control character, or a second rename while one is pending
is refused with a reason, and nothing is sent. The server's
`METADATA_CHANGED` renames the session in that coordinator's list in place,
so the switcher's row and, for the active coordinator, the header follow it.
Renames other clients make reach the list once this client has renamed a
session on that connection (it subscribes then); until then the next
workspace refresh carries them.

The panel talks to the engine over its own request and completion slot:

| Offset | Request `cockpit.session` |
|---|---|
| 0 | version 1 |
| 1 | kind: 1 describe the session on screen, 2 rename it, 3 the last rename's outcome |
| 2 | name length: required for rename, 1 to 255; empty otherwise |
| 3 | new name, UTF-8 |

| Offset | Reply |
|---|---|
| 0 | version 1 |
| 1 | phase: 0 ready, 1 pending, 2 renamed, 3 refused, 4 unavailable |
| 2 | name length, then the session's current name |
| then | host length, then the host ("This Mac" or the registered host's label) |
| then | reason length, then the reason (refused and unavailable) |

A pending rename is settled by that coordinator's drain, which announces;
the core then asks for the outcome with each snapshot until it is renamed
(the panel closes) or refused (the panel keeps the typed name and says why).
A rename whose connection ended first reads as refused, outcome unknown.

### Empty sessions

A keep-empty session ([ADR-0105](../../../ADR/0105-sessions-can-outlive-their-last-window.md))
survives its last window, so a coordinator can hold a session with no
windows. phux-client-ffi reads the mark from the snapshot's trailing
keep-empty list ([L1.md](../../../docs/spec/L1.md) section 9.1) and reports
it through `phux_client_session_flags` (KEEP_EMPTY, and EMPTY when the
session has no windows) only when HELLO_OK advertised `KEEP_EMPTY_SESSIONS`
(0x00020000); an older server's sessions always read 0. Its switcher row reads
`Empty session · <host>` instead of `Phux session`.

Cockpit shows such a session as an Empty session state with New Tab
(`src/cockpit/native/empty_session.zig`), never as a broken session. A window
shows it for the active coordinator's attached session when that session is
empty and the window holds no tab, and for a peer's empty session picked in
the switcher, in the window it was picked in. The state is a snapshot
extension record (kind 4: window mask, flags, name, host); it gives way to
every modal, and a picked one can be dismissed.

Picking a peer's empty session does not attach it: an empty session has no
tab to display, and a peer attaches only what it displays. New Tab
(`cockpit.session` kind 4) is what shows it. The active coordinator's own
empty session takes an ordinary new tab. A peer restarts only its own
connection to attach that session by id (no other coordinator redials), and
once its empty workspace projects, the tab spawns on that peer through its
own edit queue. Until the tab lands the peer is held shown; the session has no
panes then, so the attach sizes nobody's panes. If the spawn is refused, or
the peer's connection ends first, the hold ends and a peer with nothing on
screen returns to listing, as any hidden peer does.

### A failed coordinator

A peer whose connection fails or closes, or whose channel cannot open, is
marked failed until it lists again. Its group then shows one row, named for
the host, reading "Unavailable" with the recorded reason (a remote host's
dial or connection failure), else "the connection was lost". Picking that
row dials that peer again (`Engine.retryPeer`); the row then reads
"Connecting…" and is not selectable until the peer fails again or lists. A
showing peer's tabs keep their last frames, frozen, while it is down.

A listing peer that fails is also dialed again automatically
(`Engine.onPeerRetryTimer`, one timer per slot): 1 s after it fails, then
after twice the last wait each time it fails again, at most 60 s. Once it
lists, the next failure waits 1 s again. The redial is a lister's, so its
connection asks GET_STATE and never attaches. A timer that fires after the
peer listed, was picked, was removed, or began showing does nothing. A peer
that fails while showing a session is not redialed automatically: its tabs
are on screen with their frozen frames, and picking its row retries it.
Once its tabs leave the screen it returns to listing, and the backoff
applies from then on. If its
server ends the shown session instead, its tabs leave and it goes back to
listing on a fresh connection. A showing peer whose workspace cannot be
projected reads "workspace unavailable" on its session rows.

## Known limits

- At most four coordinators: the active one and three peers. The bound is
  the runtime's fixed table of eight effect channels (`max_effect_channels`
  in the pinned Native SDK), which also holds the core's engine channel, the
  active coordinator's (102) and the pointer channel (103). With three peers
  the steady state is six of eight. Each peer holds one channel, and a restart or a
  Disconnect followed by a Connect briefly holds a second while the old
  one's close is delivered; so does an automatic redial of a peer whose
  failed channel's close has not been delivered yet. If the table is ever
  full, the runtime refuses the open with a `.rejected` event: that peer is
  marked failed and redialed on its backoff, and no other coordinator is
  touched. Connecting to a fifth is refused with that reason, and nothing
  changes.
- A new tab or split that a peer has spawned but not yet placed is dropped
  from Cockpit's queue if the peer stops showing, restarts or fails first
  (for instance, choosing another coordinator's tab within the spawn's round
  trip). Its terminal keeps running on that host, unplaced. Keeping the peer
  attached until the placement lands would hold a viewport for a peer that
  is not displaying, so Cockpit does not.
- A peer that lists and then fails again, repeatedly, is redialed 1 s after
  each failure: listing resets its backoff.
- Relaunch restores which hosts are held, not what was on screen: each
  remembered host comes back listing, and none of its sessions is shown
  again until one is picked. Cockpit keeps no client-side placement for a
  Phux coordinator across relaunch (for the active coordinator as for a
  peer): which native window each of its tabs was in and which tab was
  selected come from its shared workspace when it is shown again.
- New Window and the available-terminal inventory belong to the active
  coordinator, whichever pane is focused. A peer's edit receipt is applied
  once the edit is queued on that peer; its confirmation or refusal shows in
  the peer's own projection and switcher rows, not as a command result.
- A peer is shown only while one of its tabs is on screen. Keeping a peer's
  tab in the background while another coordinator's tab is selected is not
  possible: choosing that other tab returns the peer to listing.
- A showing peer's placements are not saved as attachment evidence; they
  are projected again from its workspace when it reconnects. A peer that
  fails while showing a session is redialed only by picking its row, or
  once its tabs leave the screen and it lists again.
- Showing a peer session is refused while the active coordinator has a
  retarget pending, since the two briefly share a coordinator id.
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
