---
audience: agents, contributors
stability: evolving
last-reviewed: 2026-09-11
---

# Go to Directory

**TL;DR.** Go to Directory (`cmd+shift+J`, or Window > Go to Directory…)
browses the child directories of the connected coordinator's host and opens
a new tab whose shell starts in the chosen one. The listing is the server's
`LIST_DIRECTORY` / `DIRECTORY_LISTING` query (docs/spec/L3.md section 4), so
it names directories on whichever host Cockpit is attached to: this Mac or a
registered remote host. phux-client-ffi retains one listing per client and
drops late replies; the engine pages and filters it four rows at a time.
The layout is unchanged: the new tab is placed like New Tab.

## Using it

The picker opens in the window that invoked it. It starts in the focused
terminal's directory when that terminal lives on the host being listed, and
otherwise in that host's serving user's home. A local PTY's directory is on
this Mac, so the coordinator never lists it.

### Satellite panes

Opened over a satellite pane (a terminal the coordinator relays from one of
its satellites), the picker lists that satellite, the way the TUI does
(docs/spec/L3.md section 4.1, ADR-0108). It needs the hub to advertise
`LIST_DIRECTORY_HOST` (0x00080000):

| Hub | Request | Heading | Open Here |
|---|---|---|---|
| advertises the bit | `LIST_DIRECTORY` with `host` = the satellite, starting at the pane's catalog directory, else the satellite's home | Go to Directory on `<satellite>` | on the satellite: the pane owns the spawn, relayed as `SPAWN_RESOURCE.satellite` |
| does not | no `host`; the coordinator's own home | Go to Directory on the coordinator, not `<satellite>` | on the coordinator, without an owner |

An older hub ignores `host` and lists itself, so phux-client-ffi refuses a
host without the bit (`phux_client_list_directory_on` returns
`INVALID_STATE` and queues nothing). The engine never sends one in that case,
and says whose directories it shows instead. The host is fixed when the
picker opens: descending, going up and Open Here all stay on it until the
picker is opened again. An Open Here whose pane is no longer attached is
refused, never opened ownerless on the coordinator. A relayed refusal
(unknown or unreachable satellite, the relay deadline) is an ordinary
refused listing, and its message names the host.

| Row or key | Effect |
|---|---|
| Open a new tab here | New tab in the listed directory |
| `..` | List the parent directory |
| a directory | Enter lists it; its tab opens from its own listing |
| typing | Filters the listing by name, ignoring case |
| arrows | Move the highlight, across pages |
| Open Here | New tab in the listed directory |
| Escape or Cancel | Close; a reply still in flight is dropped |

The synthetic rows appear only while the filter is empty. Hidden directories
are listed, as the server returns them. A server that lists more than 1024
children, or stops reading early, says so under the list. A refused listing
(missing, not a directory, permission denied) names the reason and still
offers `..`. A coordinator that does not advertise `LIST_DIRECTORY` is named
as such immediately, because an older server would drop the query.

A coordinator listing's tab is spawned without an owner terminal. The
directory came from the serving server's own filesystem, and the focused
terminal's satellite route must not carry it to another host. A satellite
listing's tab is owned by the pane it was listed for, so it opens on that
satellite.

## Layers

```
core.ts --cockpit.directory--> directory_picker.zig (engine, owning thread)
                                 | requestDirectory / directoryInfo / Entry
                                 v
                               PhuxProvider -> host.zig -> phux-client-ffi
                                 phux_client_list_directory     (queue)
                                 phux_client_directory_info     (read)
                                 phux_client_directory_entry_get
```

- phux-client-ffi (`include/phux/client.h`, "host directory listing") is
  additive to ABI version 2. The client keeps exactly one listing. A new
  request replaces it, and a reply answering any request but the latest is
  dropped silently. A correlated ERROR settles the listing as refused, and
  disconnecting while it is pending makes the outcome unknown. Request IDs
  share the embedder's host request space with spawns and workspace
  requests.
- The Phux host reports `directory_changed` in its drain delta when the
  listing settles. The engine then announces an invalidation, and the core,
  still awaiting, asks for the page again.
- The engine composes every path. The core names rows by their index in the
  listing, together with the request ID that produced it. An action against
  a listing the user has already left is refused (`StaleListing`) rather than
  applied to another directory.

## The `cockpit.directory` request

It has its own completion slot in the extension bridge, delivered after every
other slot. All integers are little-endian.

| Offset | Request |
|---|---|
| 0 | version 1 |
| 1 | kind: 1 open, 2 page, 3 descend, 4 parent, 5 open a tab here |
| 2 | request ID, u32, of the listing the action applies to |
| 6 | page offset, u16 |
| 8 | row index, u16: an entry, or 0xffff for the listed directory (kind 5) |
| 10 | query length, u8, at most 64 |
| 11 | query UTF-8 |

Every reply is the page that the action leaves behind:

| Field | Reply |
|---|---|
| version | 1 |
| status | 0 unsupported, 1 pending, 2 listed, 3 refused, 4 unknown, 5 unavailable |
| request ID | u32, the listing the rows belong to |
| flags | bit 0: truncated |
| error | the wire DirectoryErrorCode when refused |
| total, offset | u16 each: matching rows, and the echoed offset |
| path | length u8, then the path, elided at 240 bytes |
| query | length u8, then the echoed query |
| rows | count u8, then per row: index u16, flags u8 (bit 0 symlink), name length u8, name |
| message | length u8, then the server's text, elided at 240 bytes |
| scope | u8: 0 the coordinator, 1 a satellite, 2 the coordinator in place of a satellite |
| host | length u8, then the satellite the picker was opened over (empty for scope 0) |
| via | length u8, then the coordinator the listing came through, when it is not the active one (empty otherwise) |

### Panes of another coordinator

With several coordinators held (docs/REMOTE_HOSTS.md, "Several
coordinators"), the listing belongs to the coordinator that minted the
focused pane, fixed when the picker opens. That coordinator's provider asks
LIST_DIRECTORY and its own feature bits decide the satellite relay, so a
satellite name is only ever resolved by the hub that relays that pane: two
coordinators that both federate a `devbox` never answer for each other. A
listing through a coordinator other than the active one names it (`via`):
the heading reads "Go to Directory on <satellite> via <coordinator>", or
"on <coordinator>" for its own host. New tabs open on the active coordinator
only, so such a listing has no Open a new tab here row, the Open Here button
says so instead of sending anything, and the engine refuses one that arrives
anyway (`OtherCoordinator`). A refused Open Here is reported as the terminal
limit only when capacity refused it.

Row index 0xffff is "Open a new tab here" and 0xfffe is `..`; the core
supplies their labels. The core accepts a reply only while the picker is
open. It also requires the reply's request ID to match its own, unless it
has just started a new listing, and the offset and query to echo its current
ones. A reply that arrives after Escape is therefore dropped.

## Validation

```sh
cargo nextest run -p phux-client-ffi -E 'test(/directory/)'
cargo run --locked -p phux-client-ffi --example cockpit_fixture --profile ffi-dev
just cockpit-test
cd clients/cockpit
node --import ./src/tests/navigation-loader.mjs --test ./src/tests/directory.test.mjs
```

The fixture example regenerates `hello_directory.bin` and
`directory_listing.bin` and validates them through the C ABI. The engine
tests in `src/tests/directory_picker_tests.zig` drive the real provider
through those frames. None of these is live macOS or remote-network
acceptance.
