---
group: internals
order: 3
title: Authority over pages, windows, and displays
---

# Authority over pages, windows, and displays

Who may watch or control a mirrored browser page, application window, or
whole display. Watching a display shows every application on the machine
and input to it controls the machine, so these grants are separate from
terminal grants and from each other. Granting a device is in
[Remote machines](./remote.md#who-can-do-what).

## Control classes

A producer declares a class when it opens a document: `page` (`phux agent
page mirror`), `window` (`phux agent surface mirror App`), `display`
(`phux agent surface mirror display[:OUTPUT]`), or none. `tree open`, `doc
open` and `surface open` declare none unless `surface open --control CLASS`
says so. `phux ls --json`, `phux resource show --json` and `phux_documents`
report it as `control`.

Only the owner socket opens a document that declares a class. A scoped
grant opens a classless tree, surface or document beside a Terminal it may
bind (`CREATE` on its Group, `BIND` on it), over the Unix socket or
localhost only. A delegated agent holds no `CREATE`, so it opens none.

## Watching

Attaching to a window or display, snapshotting it, or subscribing to its
events needs `OBSERVE` on a grant that names the class, plus `BIND` through
the parent pane:

| Caller | Watches |
| --- | --- |
| The owner socket | Every document |
| `observe@window`, `observe@display` | Windows, or displays, on this host |
| `observe@window:HOST`, `observe@display:HOST` | Windows, or displays, on satellite `HOST` |
| `observe@resource:KEY` | That one document |
| Any other grant, including one on the parent pane | No window or display |

`observe@display` does not reach a window, nor `observe@window` a display.
A `page` or a classless document is watched through its parent pane's
grants. Inventory still lists a document the caller cannot watch. A refused
attach names the missing grant and the owner's remedy (`phux workload grant
sha256:... observe@display`). A hub learns a satellite document's class from
the satellite's inventory and refuses before the link carries anything.
Removing a grant with `phux workload ungrant` ends live connections that
relied on it.

## Control

`SEND_DOCUMENT_INPUT` is allowed for:

| Caller | Reaches |
| --- | --- |
| The owner socket | Every document |
| `input@page`, `input@window`, `input@display` | Documents of that class on this host |
| `input@CLASS:HOST` | Documents of that class on satellite `HOST` |
| `input@resource:KEY` | That one document, if it has a class |
| Any other grant, including one on the parent pane | Nothing |

`?input@CLASS` holds each input for approval
([security](./security.md)). A classless document takes input only from the
owner socket. A hub stamps the classes it allowed on the relayed command,
and the satellite refuses input outside them. A satellite older than
control classes ignores the stamp, so there the hub's check is the only
gate.

## Relayed identity

A hub relaying input also names the sender: its credential id on the hub
(`owner` for the hub's socket) and client name. The satellite uses it only
for the control notice and its input log, and believes it only from its
owner socket, a credential holding every verb on `global` (the ceiling
`phux host add` pairs a hub with), or a person's device paired with the
legacy six-verb ceiling. It never authorizes: the satellite checks
the hub's link credential, and the hub checks the sender's grants.

## Paired devices

A person's device paired with the full-control ceiling (`phux host add`, a
`phux pair --enroll` ticket, or `phux workload add-key --principal human`)
also gets `observe@display` and `observe@window`. An automation key never
does.

## On the wire

The class is `SPAWN_RESOURCE` field 18 and `RESOURCE_STATE` field 6
(`page` 1, `window` 2, `display` 3; absent is none). The hub's stamp is one
trailing byte on `SEND_DOCUMENT_INPUT`, bit `1 << class` per allowed class
(`0xff` for all), followed by the relayed credential and name, each at most
255 bytes. An older peer ignores them.

## In Rune

A surface whose document declares a class takes clicks at the frame pixel
under the pointer (letterbox presses send nothing), a press that moves 4
points or more as `Drag`, the wheel as `Scroll`, and, while focused,
printable keys as `Type` and Enter, Tab, Backspace, Escape and arrows as
`Key`. Cmd and Ctrl chords stay Rune's. A classless or refused surface is
view-only and says why once.

## The clipboard

Paste and copy are input under the same grant. `Paste` puts up to 1 MiB on
the host clipboard (`pbcopy`, or `wl-copy`) and presses the paste chord; a
page inserts the text. `CopyClipboard` returns the host clipboard, at most 1
MiB, to the requesting viewer only. `CopySelection` presses the copy chord
first (or cut, with `cut`), and `SelectAll` the select-all chord. From the
CLI: `phux agent page paste|copy`; in Rune: Cmd+V and Cmd+C (Ctrl+Shift+V
and Ctrl+Shift+C on Linux) over a surface pane.
