# Phux Rune on a phone

The phone app shares this client's Desk, terminal runtime and Rune view.
There are two hosts: the desktop-hosted `--phone` form and a native UIKit
app using upstream Rune's `embed::LayerHost` and Metal layer. No Rune fork
is required. See [Native iOS](./ios/README.md) for building and qualification.
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
| Native iOS simulator app: signed bundle, real terminal rendering and native `insertText` round trip; a linked device bundle is also built in CI (not provisioned/device-qualified) | `ios/build.py`, `ios/qualify.py`, `ios/Sources/`, `ios/bridge/` |
| Native system input backed by UIKit `UITextInput`: terminal raw capture, staged composition, app-owned editor snapshots with UTF-16 validation and owner/revision rejection; the desktop-hosted phone retains its drawn keyboard | `ios/Sources/RuneInput.swift`, `src/desk/native_input/` |
| Native safe-area and keyboard avoidance through UIKit layout guides; no fake status/home insets or drawn keys in the native host | `ios/Sources/App.swift`, `src/ui/phone.rs` |
| Direct native QUIC/WSS connection: shared-runtime enrollment with an iOS-held P-256 key, pinned CA/leaf, issued client certificate and Keychain token. No CLI discovery, desktop updater or local-server bootstrap on device | `src/mobile.rs`, `ios/Sources/NativeIdentity.swift`, `ios/Sources/ConnectionForm.swift` |
| Direct-install packaging without TestFlight: exact ad-hoc profile/certificate/device coverage, verified signature and HTTPS OTA site; installation and real-device qualification are separate | `ios/package_ota.py`, `ios/README.md` |
| Native first-run Scan QR / explicit Paste / Tailscale computer discovery; shared strict link parsing, peer confirmation, automatic enrollment and connection, ticket-free same-key relaunch qualification. Tailscale discovery is a separate embedded node; SSH and terminal traffic require the installed VPN. Physical camera and account sign-in remain device qualification | `ios/Sources/OnboardingController.swift`, `ios/Sources/PairingFlow.swift`, `ios/Sources/TailscaleSetupController.swift`, `ios/Tailnet/`, `src/mobile.rs` |
| The terminal on its own ground, framed by tinted glass controls without covering its text or last-row prompt | `src/ui/phone.rs` (`glass`), `src/ui/spatial.rs` |
| Top bar: sessions (back), a capsule with the terminal and its session (tap to switch), and a compact split map or single-pane commands (⋯). Commands remain on the accessory bar in either case | `src/ui/phone.rs` |
| Every pane of the active split tab, in its literal two-dimensional arrangement: a spring camera fills the stage with the focused pane without scaling glyphs; only that pane is resized or marked seen | `src/desk/layout.rs`, `src/ui/spatial.rs` |
| A small top-right pane map, inline in the header: tap its exact split regions, or swipe the map / dedicated Move handle to one spatial neighbor. Move, Panes and Scroll have a reserved 44-point toolbar outside the measured terminal viewport. Boundary and cancelled drags spring back. Panes opens a named, keyboard-accessible picker for tiny regions | `src/desk/phone/spatial.rs`, `src/ui/spatial.rs` |
| A dedicated Scroll handle: drag for terminal history; alternate-screen programs receive arrows and mouse-tracking programs receive wheel events through the existing application-aware policy | `src/desk/phone/spatial.rs`, `src/desk/mouse.rs` |
| Tab pills and a new tab | `src/ui/phone.rs` |
| A tap on a URL or OSC 8 link opens it; a finger that travels past a few points is a selection instead | `src/desk/links.rs` |
| Accessory bar: ⌘ commands (Done in Settings search), esc, tab, one-shot ⌃ ⌥ ⇧ latches, a drag pad that sends arrows, the keyboard toggle; commands respect the active modal | `src/ui/phone.rs`, `src/desk/phone.rs` |
| Keyboard: letters, digits and symbols, shift, delete, space, return; types into the terminal, keeps focus while editing palette/find/Settings search fields, and answers the confirm bar | `src/ui/phone.rs`, `src/desk/phone.rs` |
| Type into rename, machine setup and inbox reply sheets with the drawn keyboard, including 320×568 previews. Sheets reserve keyboard/accessory space; synchronous field edits and stale-question checks keep input out of the terminal behind them. Native UIKit editing uses the same field handlers | `src/desk/native_input/`, `src/desk/phone/runtime/sheets.rs`, `src/ui/phone.rs` |
| The command palette as a sheet over the terminal, above the keyboard; paths open there too | `src/ui/palette.rs` |
| Settings: grouped category drill-down, readable phone-sized controls, current values and bottom search. Overflow fades only hidden edges; keyboard-revealed controls stay clear. Search brings up its own keyboard; Done/Hide preserves the query and the terminal's keyboard preference. Theme selection uses the existing live-preview palette and returns to Settings | `src/ui/settings.rs`, `src/ui/settings/`, `src/desk/settings.rs` |
| Machines, the inbox and the reconnect banner, narrowed to a phone's width | `src/ui.rs` (`Sheet`), `src/ui/inbox.rs`, `src/ui/status.rs` |

## Stand-ins and what replaces them

| Stand-in | Waits for, upstream | Then |
| --- | --- | --- |
| The drawn keyboard in desktop `--phone` | Native UIKit host already replaces it on iOS | Keep it for desktop phone qualification; qualify real OS keyboard/CJK/dictation on devices before claiming them |
| Glass drawn with `backdrop_blur`, four surfaces a frame | Native in-window glass on iOS (`SystemMaterial::Glass` only shows what is behind the window today) | Put the system's glass under the same surfaces |
| Fixed status/home insets and keyboard height in desktop `--phone` | Already replaced by UIKit layout guides in the native iOS host | Keep desktop snapshots deterministic; qualify rotation, floating keyboards and physical-device safe areas |
| No installed device/TestFlight distribution yet; Android remains a desktop preview | Apple provisioning/signing and device qualification; an Android activity/lifecycle host | Install and validate a provisioned iOS build before a TestFlight lane; Android is a separate host project |
| First-finger presses and drags; extra fingers are ignored. Navigation and history have dedicated handles so they never steal a program's terminal mouse/selection drag | Multi-touch gesture arbitration, long press and edge gestures (a secondary click still opens the desktop context menu) | Replace dedicated handles only when navigation/history gestures can be distinguished safely from terminal input; long press for the context menu |
| No haptics | A haptics API | A tick on latch, tab switch and pad step |
| No native Rune VoiceOver element bridge, link opener or file/image clipboard integration | App/platform adapters over upstream's runtime | Qualify the native accessibility tree and service these through UIKit; desktop accessibility is not iOS evidence |

## Next, no upstream needed

| Piece | Today |
| --- | --- |
| Phone sheets for machines and the inbox | The desktop's, narrowed |
| A suggested-commands home for the palette (sessions, new tab, new session, split) | The full ranked list |
