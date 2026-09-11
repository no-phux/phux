---
audience: agents, contributors
stability: evolving
last-reviewed: 2026-09-10
---

# Remote hosts

**TL;DR.** Connect to Host (`cmd+shift+O`, Window > Connect to Host…, or
the switcher's Connect to Host button) points Cockpit's active Phux provider
at a host in the phux CLI's own `[[remote]]` registry, resolved the way
`phux --remote HOST` resolves it. phux-client-ffi's remote tunnel dials QUIC
or TLS WebSocket with the pinned certificate and bearer token, then relays
frames through a Unix-domain socket pair. The socket worker and session
kernel are unchanged. This Mac's coordinator stays connected beside the
host, so the switcher lists both as host groups ("This Mac" first). Picking
the other group's session makes that coordinator active. Disconnect removes
only the host's group. The host is remembered beside the state file and
reattached beside this Mac on the next launch.

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

## Side by side

The model holds the active Phux provider (`Model.phux_provider`) and at most
one standby coordinator (`Model.phux_peer`). The standby is this Mac's
coordinator while a remote host is active, and the remote host while this Mac
is. It runs its own socket worker on its own channel
(`phux_peer_channel_key`, 104), so either one restarts alone. Every terminal
surface belongs to the active provider.

The standby never attaches. An attached client is a subscriber: under the
server's default `window-size = smallest` its viewport would size every pane
of its session for everyone else, and every pane's output would stream to it
for nothing. After HELLO_OK it instead asks GET_STATE
(`phux_client_query_sessions`), once per connection and again whenever the
switcher refreshes, so it sizes no pane and streams nothing. Its group lists
only while that list belongs to the current connection and no retarget is
pending. On disconnect, and the moment it is retargeted, the standby forgets
the list and its generation, so rows captured from it stop resolving. They
cannot attach a session by name on the wrong host.

| Action | Active | Standby |
|---|---|---|
| Connect to Host from this Mac | the host | this Mac (created on first use) |
| Connect to Host from a host | the new host | this Mac, unchanged |
| a session in the other group | that coordinator, that session | the one it left |
| Use this Mac | this Mac | the host, still listed |
| Disconnect | this Mac | none: the host's group goes |

Each move retargets the providers through `requestRetarget` and restarts
them through the path Reconnect uses. Frozen canvases, session handoff and
command fencing therefore behave as for any host switch, and both
connections redial. The switcher lists this Mac's sessions first, then the
host's, whichever is active. Standby rows carry a `peer_session` target held
against the standby provider's own context, so an exchange or a Disconnect
invalidates rows captured before it.

## Known limits

- One remote host at a time, beside this Mac. Connecting to a second host
  replaces the first, and this Mac stays listed.
- The standby lists sessions only. Its terminals are not in the switcher's
  terminal rows, and activating one of its sessions redials both
  connections rather than handing over live replicas.
- A standby session is selectable only by name, because that is how the
  retargeted connection attaches it. An unnamed session is listed but
  cannot be picked.
- A standby that fails to connect is not reported in the status line. Its
  group simply disappears until the standby reconnects and lists again.
- Catalog search matches titles, directories and sessions, not the host
  label.
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
