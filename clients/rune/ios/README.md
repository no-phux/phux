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
new project generator. A small Swift package supplies the Tailscale directory
and SSH client; its pinned dynamic TailscaleKit framework is embedded and
signed inside-out. Builds resolve these dependencies into the output path,
not the checkout. The build derives Rust's ordered native link flags, bundles
Cargo, Swift and Go license notices, and ad-hoc signs the simulator app.
It compiles a small asset catalog with `actool`: a launch colour matching
the terminal ground, and `ios/AppIcon.png`, which is
`packaging/AppIcon.png` flattened onto its own ground (an iOS icon may not
have transparent corners). Use `--release` for a build meant for a phone:
a debug build of the renderer is not representative of how it scrolls. The device bundle
is linked **without provisioning or distribution signing**, not installable
device qualification. Do not install it over another app or use production
Phux to run a test.

### Direct installation without TestFlight

`build.py --target device` still produces an unsigned bundle. To make an
installable preview, obtain an **ad-hoc distribution** profile for the exact
`sh.phux.rune.ios` App ID, the existing Apple Distribution certificate and
each intended phone's registered UDID. An old phone's profile does not
authorize a newer phone merely because Xcode knows its name.

```sh
python3 clients/rune/ios/package_ota.py \
  --app '/absolute/scratch/ios-device/Phux Rune.app' \
  --profile /absolute/private/Rune.mobileprovision \
  --identity APPLE_DISTRIBUTION_CERTIFICATE_SHA1 \
  --device-udid REGISTERED_PHONE_UDID \
  --base-url https://private.example/rune-preview/ \
  --out /absolute/scratch/rune-ota
```

Host **only the generated `site/` directory** at that HTTPS URL. It contains
the IPA, manifest and Safari installation page; the rest of the output is
not web content. Private Tailscale HTTPS hosting is one option; verify the
page, manifest and complete IPA download before sharing the link. The phone
must be able to reach it. Serve `.plist` as `text/xml` and `.ipa` as
`application/octet-stream`. Do not expose server credentials or pairing tickets
in the page, manifest or IPA.

Packaging rejects expired/development/store/enterprise profiles, missing
device coverage, wrong bundle/certificate, simulator binaries and unsupported
nested executables/symlinks. Only the expected device TailscaleKit framework
is admitted; it is signed without app entitlements before the app. Packaging
signs using the local keychain without exporting
the private key, verifies the signature and exact authorized entitlements,
and never installs an app, changes provisioning or uploads to TestFlight.
The user must approve installation on the phone. A signed IPA and reachable
install page are **not** proof that the app installed, launched or passed
physical-device qualification.

`qualify.py` creates and deletes its own exact simulator UDID, selects a
compatible available runtime/model pair, and owns a same-source server on a
short temporary socket with clean configuration. All commands are bounded.
It mints a one-use ticket on a private `paired` server, imports the QR payload
through the canonical Rust parser and native confirmation operation, enrolls
through the shared runtime using a simulator Keychain-held P-256 signing key, then dials
QUIC with the issued client certificate, pinned authority and token. Pairing
secrets travel through a private app-container file, not launch arguments,
logs or evidence artifacts. It injects a command through the actual UIKit
`insertText` entry, requires
the returned marker in a real terminal grid and presented frame, records the
element tree/report/screenshot and fifteen UIKit input/onboarding-contract groups, and
backgrounds/resumes the app through the
OS before checking continued rendering. The report must also say the surface
shows its drawable at the screen's scale: Rune reports its own geometry, not
how the layer presents it. A fresh process then reconnects from
saved settings and the same Keychain key, with no ticket or pairing payload.
It never adopts a user's simulator
or production socket. Qualification-only socket/command launch arguments
are compiled out on devices.

The initial launch never requests termination: the fixture owns a newly
created simulator with a freshly installed app, so no previous process can
exist. This avoids a redundant SpringBoard termination/registration round
trip on cold CI images. All launch deadlines and the real paired input and
lifecycle requirements remain unchanged.

A just-booted simulator keeps doing first-boot work for minutes after
`bootstatus` returns; on CI runners install, launch and `launchctl` calls in
that window took 20-130 seconds and the app's 18-second connect budget ran
out. Before installing, the fixture waits until three consecutive
`launchctl list` queries each answer within two seconds, for at most five
minutes, and records the query times in `settle.json`. Still slow at the
budget, it says so on stderr and continues under the per-command deadlines.

Cold CoreSimulator registration can outlast installation. Container acquisition
uses bounded timeout-only retries (at most three 60-second attempts) and must
return an existing absolute directory. The fixture retains that Documents path
instead of asking LaunchServices again after launch. Each command records its
duration, deadline and success without credential contents. Application launch,
report and lifecycle deadlines are unchanged; no failed app assertion is retried.
Command stdout and diagnostic stderr are stored separately so service warnings
cannot be mistaken for an app-container path or JSON response.
Report and tree validation reads the retained evidence copies, so the files
uploaded by CI are the exact documents that passed qualification.

**Evidence boundary:** calling the UIKit input override is not physical
keyboard, autocorrect, dictation or CJK IME automation. A simulator screenshot
and device link are not real-device GPU, battery, accessibility or TestFlight
proof. The reusable Rune CI lane requires native simulator qualification over
both paired QUIC and WSS (enrolling over QUIC in either case),
as well as the existing desktop and desktop-hosted phone checks.

## Connect

The app is iPhone-only for now: a fixed portrait form, declared as such in
the bundle (`UIDeviceFamily` 1) and its asset catalog. It opts out of the
phone's 60 Hz display-link cap (`CADisableMinimumFrameDurationOnPhone`).
Setup is dark on a light-mode phone too, alerts included, like the terminal
behind it.

First run offers **Scan pairing QR**, **Paste pairing link**, and **Find with
Tailscale**. On your computer, `phux pair --enroll` prints the code and link.
Scan the QR, or paste its whole link: the row's Paste button never asks, and
tapping the row itself reads the clipboard behind the system's one prompt. An
empty clipboard or anything that is not a pairing link is answered with an
alert, never silence.

A staged pairing takes the whole screen: **Pair with _name_?**, the host it
will connect to, a filled **Pair & Connect** button and **Cancel**. The other
ways to connect are hidden until that is answered, and nothing is enrolled or
replaced before the button. Enrolling shows progress alone. No endpoint, pins,
token or ticket transcription is needed. The old credential fields remain
**Advanced setup**, labelled, where **Enroll & Connect** saves and opens the
computer in one step.

A pairing that fails interrupts with an alert, an error haptic and a VoiceOver
announcement, worded by what went wrong: the computer could not be reached
(the same code is offered again), the code expired or was already used, this
phone could not create its key, or anything else. The raw error is never
shown; the class is recorded as `enroll_unreachable`, `enroll_code_rejected`
or `enroll_device_key` in the support file described below. A valid scan and a
completed pairing answer with a success haptic.

Once a computer is saved the screen is titled **Change computer**, names the
computer this phone is paired with, and says that pairing another replaces it.
The name is a preference of its own, beside the saved configuration.

The camera requires permission and supported hardware; denied/unavailable
camera access leaves Paste available. The scanner always shows what to scan,
says so in the same caption when a code is not a pairing code, keeps scanning,
and offers the flashlight where the phone has one. Cold and warm `phux-rune://connect?…` links use the same confirmation
path. Rune does not claim the other mobile app's `phux` scheme or HTTPS
Universal Links. Current and legacy Phux pairing URLs can be scanned/pasted.
Every import is bounded, rejects ambiguous credentials and stages a complete
candidate without borrowing trust or secrets from an existing connection.
Direct QUIC-only links are accepted without inventing a WebSocket fallback.
Qualification covers the actual Swift SSH-document converter through the C/Rust
importer, as well as live QUIC-only import and enrollment. A rejected import is
reported before leaving the Tailscale picker; obtaining a code is not success.

### Find with Tailscale

Sign in inside Rune and choose an online Mac or Linux computer. Rune adds
its own discovery-only node to your tailnet; this is separate from the
installed Tailscale app. Device approval, ACL visibility and device limits
still apply. Discovery runs while its picker is open; local node state is
excluded from backups and SDK log uploads are disabled before startup.
**Sign out of discovery** leaves the Tailscale app signed in and this phone
paired. Rune closes Tailscale's sign-in page itself once sign-in or approval
lands. Rows that cannot be chosen say why and do not respond: an offline
computer, one that is not a Mac or Linux machine, a connected VPN.

The installed Tailscale app **must also be connected to the same tailnet**:
SSH, enrollment and terminal sockets use that VPN, not the embedded directory.
Enter the computer's local account name, not its owner's Tailscale email.
Rune remembers only account names that completed SSH, never guessed email
prefixes or passwords. Tailscale SSH can authenticate without a
password and may ask for a browser check. Ordinary SSH can use a one-time
computer login password, never persisted—not an Apple ID password or phone
passcode. The request cannot be sent with a blank account name. Failed setup
shows an alert that can retry, go back to scan or paste, or simply be read;
so does choosing a computer while the Tailscale app is off. Credential-free phase/error codes stay locally in
`Documents/rune-onboarding-status.json` for support and are excluded from
backups; no raw SSH output, password, token, ticket or banner is recorded.
Host keys come from the authenticated netmap for
Tailscale SSH. Ordinary SSH additionally requires the full MagicDNS name to
resolve through the phone's VPN to the selected netmap address; without that
same-tailnet proof, use QR instead. A discovered IP alone is never Phux trust.

SSH only locates an existing Phux and requests `pair --json --enroll` from its
running server. It does not install, upgrade or start a server. A computer
without SSH access or a running paired QUIC listener can use QR after that
server is configured. The returned link goes through the same parser and
explicit **Pair & Connect** confirmation as a camera scan.

Enrollment runs off the UI thread through the shared runtime;
it checks the issued chain against the pinned CA and device public key.
Tickets are never saved. On devices, the private key stays in the Secure
Enclave; the simulator uses a Keychain-held `SecKey`. No private key enters
Rust. Each enrollment uses a fresh key; failed/cancelled staged keys are
removed and old working identities preserved.
If saving an issued identity fails, Retry saves that same identity rather
than consuming the one-use ticket again.
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

No native path starts `phux server --ensure`, discovers a phone-local CLI, or
runs the desktop binary updater. The SSH setup step locates the CLI only on
the explicitly selected computer. Reconnect retains the native target and trust.
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
The terminal accessory's one-shot Ctrl/Alt/Shift latches apply to system
keyboard input too. Native fields use their system keyboard modifiers;
their accessory offers Done instead of terminal latches. Tap the field to
resume editing after Done. Accessory keys never type through a focused or
held ACP editor into the terminal behind it. The native accessory is laid
out after every sheet's modal content, so its controls remain tappable;
unmounted ACP fields lose only focus/preedit, never their committed draft.

Remaining qualification/integration is tracked in Beads: provisioned device
and TestFlight, OS-driven keyboard/IME tests, physical-camera recognition and
real-device Tailscale sign-in/SSH qualification,
VoiceOver mapping, file/image clipboard, haptics beyond a hold and a copy,
hardware-keyboard repeat, landscape and Android. See [MOBILE.md](../MOBILE.md) for the desktop/native distinction.
