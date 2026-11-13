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
| Native safe-area and keyboard avoidance through UIKit layout guides; no fake status/home insets or drawn keys in the native host. No UIKit bar above Rune's own header: the surface's edge colour continues under the status bar and home indicator, the status bar and keyboard follow it, and the launch screen and app icon share the same ground | `ios/Sources/App.swift`, `src/mobile/host.rs`, `ios/build.py` (`compile_assets`) |
| The native host serves what an embedded runtime cannot reach: copied text goes to the system pasteboard, Paste reads it (the system asks first), tapped links open, and a hold or a copy plays a haptic. A surface that fails becomes a retryable state, not a dead view | `src/mobile/host.rs`, `ios/Sources/RuneView.swift` (`serve`), `ios/Sources/App.swift` |
| Direct native QUIC/WSS connection: shared-runtime enrollment with an iOS-held P-256 key, pinned CA/leaf, issued client certificate and Keychain token. No CLI discovery, desktop updater or local-server bootstrap on device | `src/mobile.rs`, `ios/Sources/NativeIdentity.swift`, `ios/Sources/ConnectionForm.swift` |
| Direct-install packaging without TestFlight: exact ad-hoc profile/certificate/device coverage, verified signature and HTTPS OTA site; installation and real-device qualification are separate | `ios/package_ota.py`, `ios/README.md` |
| Native first-run Scan QR / explicit Paste / Tailscale computer discovery; shared strict link parsing, peer confirmation, automatic enrollment and connection, ticket-free same-key relaunch qualification. Tailscale discovery is a separate embedded node; SSH and terminal traffic require the installed VPN. Physical camera and account sign-in remain device qualification | `ios/Sources/OnboardingController.swift`, `ios/Sources/PairingFlow.swift`, `ios/Sources/TailscaleSetupController.swift`, `ios/Tailnet/`, `src/mobile.rs` |
| The terminal on its own ground, framed by tinted glass controls without covering its text or last-row prompt | `src/ui/phone.rs` (`glass`), `src/ui/spatial.rs` |
| Top bar: back opens Sessions; the title opens the active tab's pane picker (Connections when there is no pane); the bell opens What's new; ⋯ opens terminal actions, not command search. The title's activity colour and state follow the focused pane. Split tabs keep their compact map and reach terminal actions from the pane picker. Commands remain on the accessory bar | `src/ui/phone.rs`, `src/ui/phone/navigation.rs` |
| Sessions: a dedicated scrollable browser with the current session and pane marked, pane/agent state and attention counts, empty sessions, search across session/pane/agent/folder names, and a named or one-tap new session. Search raises the keyboard only when tapped; Done hides it without clearing the filter or changing the terminal's keyboard choice. Connections opens native connection setup on iOS; the desktop-hosted phone also browses other machines' CLI-backed session summaries | `src/ui/phone/navigation.rs`, `src/desk/phone/navigation.rs` |
| Every pane of the active split tab, in its literal two-dimensional arrangement: a spring camera fills the stage with the focused pane without scaling glyphs; only that pane is resized or marked seen | `src/desk/layout.rs`, `src/ui/spatial.rs` |
| A small top-right pane map, inline in the header: tap its exact split regions, or swipe the map / dedicated Move handle to one spatial neighbor. With several panes, Move and Panes have a reserved 44-point toolbar outside the measured terminal viewport; a lone terminal has none. Boundary and cancelled drags spring back. Panes opens a named, keyboard-accessible picker for tiny regions | `src/desk/phone/spatial.rs`, `src/ui/spatial.rs` |
| One finger on the terminal: a drag scrolls and coasts with momentum (history, a pager's arrows or a tracking program's wheel, through the application-aware wheel policy); a hold selects by words and lifting copies; in a program that tracks the mouse a hold hands it the drag, and ⇧ keeps the selection local; a tap opens a URL or OSC 8 link, clicks a tracking program, or brings the keyboard back | `src/desk/phone/touch.rs`, `src/desk/mouse.rs`, `src/desk/links.rs` |
| Named tab pills and a new tab, once a session has more than one tab; a waiting tab carries a dot | `src/ui/phone.rs` |
| Accessory bar: ⌘ commands (Done in Settings search), a strip that scrolls through esc, tab, one-shot ⌃ ⌥ ⇧ latches, ⌃C, arrows, `\|` `~` `/` `-` and Paste, a drag pad that sends arrows, the keyboard toggle; commands respect the active modal | `src/ui/phone.rs`, `src/desk/phone.rs` |
| Keyboard: letters, digits and symbols, shift, delete, space, return; types into the terminal, keeps focus while editing palette/find/Settings search fields, and answers the confirm bar | `src/ui/phone.rs`, `src/desk/phone.rs` |
| Phone viewport changes wait for 120 ms of grid stability before resizing the PTY, avoiding intermediate keyboard/sheet sizes. Layout, hit testing and input stay immediate; first fits and desktop resizing do not wait | `src/desk/phone/refit.rs`, `src/desk/frame.rs`, `ios/bridge/src/host.rs` |
| Type into rename, machine setup and inbox reply sheets with the drawn keyboard, including 320×568 previews. Sheets reserve keyboard/accessory space; synchronous field edits and stale-question checks keep input out of the terminal behind them. Native UIKit editing uses the same field handlers | `src/desk/native_input/`, `src/desk/phone/runtime/sheets.rs`, `src/ui/phone.rs` |
| The command palette as a sheet over the terminal, above the keyboard; paths open there too. A phone lists what it reaches for first (New Tab, Paste, Find, sessions, splits, text size, Settings, Change Computer), leaves out desktop-only commands, and shows touch-height rows without chords | `src/palette/phone.rs`, `src/ui/palette.rs` |
| Settings: grouped category drill-down, readable phone-sized controls, current values and bottom search. Overflow fades only hidden edges; keyboard-revealed controls stay clear. Search brings up its own keyboard; Done/Hide preserves the query and the terminal's keyboard preference. Theme selection uses the existing live-preview palette and returns to Settings | `src/ui/settings.rs`, `src/ui/settings/`, `src/desk/settings.rs` |
| Machines, the inbox and the reconnect banner, narrowed to a phone's width | `src/ui.rs` (`Sheet`), `src/ui/inbox.rs`, `src/ui/status.rs` |

## Stand-ins and what replaces them

| Stand-in | Waits for, upstream | Then |
| --- | --- | --- |
| The drawn keyboard in desktop `--phone` | Native UIKit host already replaces it on iOS | Keep it for desktop phone qualification; qualify real OS keyboard/CJK/dictation on devices before claiming them |
| Glass drawn as a tint, rim and lift, with no blur: every surface sits beside the terminal on flat ground | Native in-window glass on iOS (`SystemMaterial::Glass` only shows what is behind the window today) | Put the system's glass under the same surfaces |
| Fixed status/home insets and keyboard height in desktop `--phone` | Already replaced by UIKit layout guides in the native iOS host | Keep desktop snapshots deterministic; qualify rotation, floating keyboards and physical-device safe areas |
| No installed device/TestFlight distribution yet; Android remains a desktop preview | Apple provisioning/signing and device qualification; an Android activity/lifecycle host | Install and validate a provisioned iOS build before a TestFlight lane; Android is a separate host project |
| First-finger presses and drags; extra fingers are ignored. The app times a hold itself from its poll clock, since a drag carries no time. Pane navigation keeps a dedicated handle so it never steals a terminal drag | Multi-touch gesture arbitration, long press and edge gestures (a secondary click still opens the desktop context menu) | Two fingers or an edge swipe for panes, and a context menu on a long press (the native host already pinches for text size through UIKit) |
| Haptics only where the native host plays them: a hold and a copy | A haptics API | A tick on latch, tab switch and pad step |
| No native Rune VoiceOver element bridge or file/image clipboard integration (text copy, paste and links go through the native host) | App/platform adapters over upstream's runtime | Qualify the native accessibility tree and service these through UIKit; desktop accessibility is not iOS evidence |

Upstream `740ecbc9` adds automatic long press, but cancels an application-owned
terminal drag at 500 ms and consumes its lift. It cannot replace the terminal's
hold-to-select/program-drag policy without a touch-ownership seam. Keep the pin
before that change until stationary holds, dragged holds and Shift-local holds
all retain their selection/copy or program-button release.

## Next, no upstream needed

| Piece | Today |
| --- | --- |
| Phone sheets for machines and the inbox | The desktop's, narrowed |
