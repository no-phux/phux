# Phux Rune on a phone

The phone app is being built inside this client, ahead of the platform.
`--phone` runs the phone form: a 393×852 window over the same runtime as the
desktop, with every piece of the phone UI working against a live server.
Whatever Rune can't do on a phone yet is drawn by the app instead, and each
of those stand-ins is listed below with the upstream capability that
replaces it. The [Rune upstream follower](../../scripts/rune-upstream/README.md)
reads this file on every run: it swaps a stand-in out as soon as upstream
ships what it waits for, and builds the next piece from the last table when
upstream is quiet.

Run it against a dev server on a temp socket, never the installed one
(see [Develop](./README.md#develop)):

```sh
cd clients/rune
env -u PHUX_SOCKET -u PHUX_TERMINAL_ID -u PHUX_PROFILE \
  cargo run -- --phone --socket /tmp/phux-test/s
env -u PHUX_SOCKET -u PHUX_TERMINAL_ID -u PHUX_PROFILE \
  cargo run -- --phone --socket /tmp/phux-test/s --snap phone.png --at 3 --size 393x852
```

Qualification (`scripts/qualify.sh`) launches the phone form beside the
desktop one, so every CI build and release proves it draws a session.

## What works

| Piece | Where |
| --- | --- |
| The terminal on its own ground, framed by tinted glass controls without covering its text or last-row prompt | `src/ui/phone.rs` (`glass`), `src/ui/spatial.rs` |
| Top bar: sessions (back), a capsule with the terminal and its session (tap to switch), and a compact split map or single-pane commands (⋯). Commands remain on the accessory bar in either case | `src/ui/phone.rs` |
| Every pane of the active split tab, in its literal two-dimensional arrangement: a spring camera fills the stage with the focused pane without scaling glyphs; only that pane is resized or marked seen | `src/desk/layout.rs`, `src/ui/spatial.rs` |
| A small top-right pane map, inline in the header: tap its exact split regions, or swipe the map / dedicated Move handle to one spatial neighbor. Move, Panes and Scroll have a reserved 44-point toolbar outside the measured terminal viewport. Boundary and cancelled drags spring back. Panes opens a named, keyboard-accessible picker for tiny regions | `src/desk/phone/spatial.rs`, `src/ui/spatial.rs` |
| A dedicated Scroll handle: drag for terminal history; alternate-screen programs receive arrows and mouse-tracking programs receive wheel events through the existing application-aware policy | `src/desk/phone/spatial.rs`, `src/desk/mouse.rs` |
| Tab pills and a new tab | `src/ui/phone.rs` |
| Accessory bar: ⌘ commands (Done in Settings search), esc, tab, one-shot ⌃ ⌥ ⇧ latches, a drag pad that sends arrows, the keyboard toggle; commands respect the active modal | `src/ui/phone.rs`, `src/desk/phone.rs` |
| Keyboard: letters, digits and symbols, shift, delete, space, return; types into the terminal, keeps focus while editing palette/find/Settings search fields, and answers the confirm bar | `src/ui/phone.rs`, `src/desk/phone.rs` |
| The command palette as a sheet over the terminal, above the keyboard; paths open there too | `src/ui/palette.rs` |
| Settings: grouped category drill-down, readable phone-sized controls, current values and bottom search. Search brings up its own keyboard; Done/Hide preserves the query and the terminal's keyboard preference. Theme selection uses the existing live-preview palette and returns to Settings | `src/ui/settings.rs`, `src/ui/settings/`, `src/desk/settings.rs` |
| Machines, the inbox and the reconnect banner, narrowed to a phone's width | `src/ui.rs` (`Sheet`), `src/ui/inbox.rs`, `src/ui/status.rs` |

## Stand-ins and what replaces them

| Stand-in | Waits for, upstream | Then |
| --- | --- | --- |
| The drawn keyboard | An iOS shell with system text input (`UITextInput`: the system keyboard, autocorrect, dictation, marked text) | Drop the drawn keys; keep the accessory bar as the keyboard's input accessory, and route its keys through `Desk::soft` as now |
| Glass drawn with `backdrop_blur`, four surfaces a frame | Native in-window glass on iOS (`SystemMaterial::Glass` only shows what is behind the window today) | Put the system's glass under the same surfaces |
| `STATUS_INSET` and `HOME_INSET` | Safe-area insets reported to the view | Pad by the reported insets |
| The keyboard's height is fixed | The system keyboard's frame and its show/hide animation curve | Size the stage by the keyboard frame on the same spring |
| A phone-sized desktop window | An iOS app shell (`UIApplication`/`UIScene` around Rune's Metal backend) and an Android activity | A `phux-rune` iOS target built in CI (`cargo check --target aarch64-apple-ios-sim`), then a TestFlight lane |
| First-finger presses and drags; extra fingers are ignored. Navigation and history have dedicated handles so they never steal a program's terminal mouse/selection drag | Multi-touch gesture arbitration, long press and edge gestures (a secondary click still opens the desktop context menu) | Replace dedicated handles only when navigation/history gestures can be distinguished safely from terminal input; long press for the context menu |
| No haptics | A haptics API | A tick on latch, tab switch and pad step |
| The CLI finds the server | Pairing without a CLI on the device | Pair with `phux pair --qr` from the app |

## Next, no upstream needed

| Piece | Today |
| --- | --- |
| Type into a sheet's field (rename, add a machine, the inbox reply) | The drawn keyboard reaches the terminal, the palette, find, Settings search and the confirm bar |
| Phone sheets for machines and the inbox | The desktop's, narrowed |
| A suggested-commands home for the palette (sessions, new tab, new session, split) | The full ranked list |
