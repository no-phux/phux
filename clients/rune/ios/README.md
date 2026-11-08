# Native Rune on iOS

UIKit owns the scene, safe-area/keyboard layout guides, display link,
system text input and Keychain. Upstream Rune's `embed::LayerHost` owns the
Metal surface and the existing Phux `Desk`/phone view. There is no framework
fork, duplicate terminal renderer, winit mobile loop or desktop CLI bootstrap.

## Build and qualify

Requires Xcode with iOS simulator SDK, Rust's `aarch64-apple-ios-sim` and
`aarch64-apple-ios` targets, plus the repository's normal build dependencies.
Minimum iOS is 26.0. Run from the repository root with absolute evidence paths
outside the worktree:

```sh
rustup target add aarch64-apple-ios-sim aarch64-apple-ios
python3 clients/rune/ios/build.py --target simulator --out /absolute/scratch/ios-sim
python3 clients/rune/ios/build.py --target device --out /absolute/scratch/ios-device
env -u PHUX_SOCKET -u PHUX_TERMINAL_ID -u PHUX_PROFILE cargo build --locked -p phux
python3 clients/rune/ios/qualify.py --app '/absolute/scratch/ios-sim/Phux Rune.app' \
  --phux "$PWD/target/debug/phux" --out /absolute/scratch/ios-proof
python3 clients/rune/ios/qualify.py --app '/absolute/scratch/ios-sim/Phux Rune.app' \
  --phux "$PWD/target/debug/phux" --transport wss --out /absolute/scratch/ios-wss-proof
```

The direct Swift/static-library build needs no generated Xcode project or
new project generator. It derives Rust's ordered native link flags, bundles
license/font notices and ad-hoc signs the simulator app. The device bundle
is linked **without provisioning or distribution signing**, not installable
device qualification. Do not install it over another app or use production
Phux to run a test.

`qualify.py` creates and deletes its own exact simulator UDID, selects a
compatible available runtime/model pair, and owns a same-source server on a
short temporary socket with clean configuration. All commands are bounded.
It mints a one-use ticket on a private `paired` server, enrolls through the
shared runtime using a simulator Keychain-held P-256 signing key, then dials
QUIC with the issued client certificate, pinned authority and token. Pairing
secrets travel through a private app-container file, not launch arguments,
logs or evidence artifacts. It injects a command through the actual UIKit
`insertText` entry, requires
the returned marker in a real terminal grid and presented frame, records the
element tree/report/screenshot and eight UIKit input-contract groups, and
backgrounds/resumes the app through the
OS before checking continued rendering. It never adopts a user's simulator
or production socket. Qualification-only socket/command launch arguments
are compiled out on devices.

**Evidence boundary:** calling the UIKit input override is not physical
keyboard, autocorrect, dictation or CJK IME automation. A simulator screenshot
and device link are not real-device GPU, battery, accessibility or TestFlight
proof. The reusable Rune CI lane requires native simulator qualification over
both paired QUIC and WSS (enrolling over QUIC in either case),
as well as the existing desktop and desktop-hosted phone checks.

## Connect

Run `phux pair --enroll` on the server. The Connection sheet accepts its
`quic://host:port`, leaf fingerprint, `sha256:` authority pin, token and
one-time enrollment ticket, plus an optional session. Tap **Enroll this phone**,
then **Connect**. Enrollment runs off the UI thread through the shared runtime;
it checks the issued chain against the pinned CA and device public key.
Tickets are never saved. On devices, the private key stays in the Secure
Enclave; the simulator uses a Keychain-held `SecKey`. No private key enters
Rust. Each enrollment uses a fresh key; failed/cancelled staged keys are
removed and old working identities preserved.
If a locked Keychain prevents removal, the public key tag is queued for
cleanup on the next activation; the original enrollment error is retained.
Replacing the saved endpoint retires its old token only after the new
credential is saved. Deferred cleanup never stores a ticket or private key.

After enrollment, the same identity can dial `wss://host/path`.
Non-loopback targets require pin, token and an enrolled client identity.
Plain `ws://` is accepted
only on loopback for local development. Credentials in URL userinfo/query
and fragments are rejected. The token lives in Keychain with
`WhenUnlockedThisDeviceOnly`; endpoint, public pin and session use preferences.
The issued public chain and key tag are preferences, not private key material.
Keychain failures are shown, not silently downgraded. Pairing credentials
are never included in qualification reports.

No native path starts `phux server --ensure`, discovers a local CLI, or runs
the desktop binary updater. Reconnect retains the native target and trust.
The scene pauses all rendering before backgrounding; worker wakes dispatch
onto the main queue, and display scheduling honors Rune's motion/deadlines.
Qualification waits for explicit native background/resumed-frame reports,
not a guessed delay after switching apps.

## Input ownership

UIKit implements `UITextInput`; Rune paints the terminal and fields. Terminal
capture keeps marked text/dictation local until committed. Paste retains the
existing confirmation boundary. App fields are edited synchronously through
their app-owned `TextEdit`, not an unaddressed deferred IME event. Snapshots
carry target identity, revision and UTF-16 selection; stale owners and split
surrogates fail closed. Unknown/inert fields never fall through to terminal
input. Return uses the same palette/search/rename/inbox/question handlers.

Remaining qualification/integration is tracked in Beads: provisioned device
and TestFlight, OS-driven keyboard/IME tests, camera/QR pairing-link import,
VoiceOver mapping, native links/file/image clipboard, haptics and
Android. See [MOBILE.md](../MOBILE.md) for the desktop/native distinction.
