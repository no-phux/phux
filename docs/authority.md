# Authority over pages, windows, and displays

Who may watch or control a mirrored browser page, application window, or
whole display. Watching a display shows every application on the machine,
and input to it is control of the machine, so window and display grants are
separate from terminal grants and from each other. A device paired to watch
terminals does not inherit a desktop. The kinds themselves are in
[Resource kinds](./resource-kinds.md); granting a device is in
[Remote machines](./remote.md#who-can-do-what).

## Control classes

A producer declares a control class when it opens a document: `page` (a
browser page, `phux agent page mirror`), `window` (one application window,
`phux agent surface mirror App`), `display` (a whole screen, `phux agent
surface mirror display[:OUTPUT]`), or none. Plain `phux agent tree open`,
`doc open` and `surface open` declare none unless `surface open --control
CLASS` says otherwise. `phux ls --json`, `phux resource show --json` and MCP
`phux_documents` report it as `control`.

## Watching

Attaching to a window or display, its snapshots (`phux agent surface
snapshot`, MCP `phux_surface_snapshot` and `phux_tree_snapshot`, which
attach), its stream, and a subscription to its own events need `OBSERVE` on a grant that names the
class:

| Caller | Watches |
| --- | --- |
| The owner socket | Every document |
| `observe@window`, `observe@display` | Windows, or displays, on this host |
| `observe@window:HOST`, `observe@display:HOST` | Windows, or displays, on satellite `HOST` |
| `observe@resource:KEY` | That one document |
| `global`, `host`, `host:NAME`, `group`, `session`, `terminal` grants, or a grant on the parent pane | No window or display |

`observe@display` does not reach a window, nor `observe@window` a display.
The attach also needs `BIND` as before, through the parent pane. A `page`,
or a document with no class, is watched through its parent pane's grants: a
browser page shows one site, not the machine. Inventory is unchanged: a
grant with `INVENTORY` still lists a display it cannot watch, with its id,
kind, parent, name and `control`.

A refused attach names the missing grant and the owner's remedy, for
example `phux workload grant sha256:... observe@display`, when the caller
could have watched it through its parent before; anyone else gets a plain
`permission denied`. Rune shows that reason in place of the mirror.

A hub admits the attach to a satellite's document on the host grant, then
learns the document's class from the satellite's inventory and refuses a
window or display the caller has no `CLASS:HOST` grant for, before the
shared link carries anything. The satellite still checks the hub's own link
credential, which `phux host add` enrolled as a paired device. A satellite
older than control classes reports none, and its documents are watched as
before.

Removing a watch grant (`phux workload ungrant`) ends live connections that
relied on it, even when a broader grant remains.

## Control

`SEND_DOCUMENT_INPUT` is allowed for:

| Caller | Reaches |
| --- | --- |
| The owner socket | Every document, whatever its class |
| `input@page`, `input@window`, `input@display` | Documents of that class on this host |
| `input@page:HOST`, `input@window:HOST`, `input@display:HOST` | Documents of that class on satellite `HOST` |
| `input@resource:KEY` | That one document, if it has a class |
| `global`, `host`, `host:NAME`, `group`, `session`, `terminal` grants, or a grant on the parent pane | Nothing |

`?input@CLASS` (or `?input@CLASS:HOST`) holds each such input for approval
instead ([Held actions](./approvals.md)). A document with no class takes input
only from the owner socket. A hub
authorizes input to a satellite's document with the `CLASS:HOST` forms and
stamps the classes it allowed on the relayed command; the satellite, which
knows the document's class, refuses one outside them, and checks the hub's
own link credential as usual. Removing a control grant ends live
connections that relied on it, even when a broader grant remains.

## Paired devices

A person's device paired with the full-control ceiling (`phux host add`, a
`phux pair --enroll` ticket, or `phux workload add-key --principal human`
with that ceiling) is also enrolled with `observe@display` and
`observe@window`: the person owns both machines. An automation key never
gets them. Devices paired before watching needed a grant were given both
once; [Remote machines](./remote.md#watching-windows-and-displays) has the
details.

## On the wire

The class is `SPAWN_RESOURCE` field 18 and `RESOURCE_STATE` field 6 (`page`
1, `window` 2, `display` 3; absent is none), and the hub's input stamp is one
trailing byte on `SEND_DOCUMENT_INPUT`, bit `1 << class` per class allowed.
An older server ignores both, and an older satellite then judges the hub's
link alone. Watching adds nothing to the wire.

## In Rune

Rune's panes send input too. A surface whose document declares
a class takes a click at the frame pixel under the pointer (contain-fit, so
a press in the letterbox bars sends nothing), a press that moves 4 points or
more as `Drag` from where it went down to where it came up (selecting text),
the wheel as `Scroll` in frame
pixels (positive `dy` scrolls the page down), and, while focused, printable
keys as `Type` and Enter, Tab, Backspace, Escape and the arrows as `Key`;
Cmd and Ctrl chords stay the app's. A document with no class, or one whose
send was refused for a missing grant or producer, is a view-only mirror: it
captures neither pointer nor keys, and the refusal is said once.

## The clipboard

A paste and a copy are input too, under the same grant. `Paste` puts text
(up to 1 MiB) on the host clipboard (`pbcopy`, or `wl-copy` from
wl-clipboard on Wayland; a missing tool is an error, never typing) and
presses Cmd+V or Ctrl+V (Ctrl+Shift+V when Hyprland's focused window is a
terminal such as kitty, foot, Alacritty, Ghostty or WezTerm; on Linux through
the virtual keyboard, else `wtype` or `ydotool`); a page inserts the text
instead. The server holds a paste up to 3 s for the producer's outcome, so a
chord that could not be pressed is an error the viewer sees; an older
producer never answers, and the paste then succeeds. `CopyClipboard`
returns the host clipboard's text, at most 1 MiB, to the viewer that asked
and no one else: the server holds the command open until the producer
answers. From the CLI, `phux agent page paste|copy`; in Rune, Cmd+V and
Cmd+C (Ctrl+Shift+V and Ctrl+Shift+C on Linux) over a surface pane; on a
phone, the paste and copy buttons beside the keyboard.
