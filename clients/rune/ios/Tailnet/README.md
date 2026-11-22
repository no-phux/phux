# RuneTailnet

Independent native tailnet directory and authenticated SSH pairing for Rune.
The app imports `RuneTailnet`; no PhuxKit or SwiftUI dependency is included.

## App contract

```swift
let directory = EmbeddedTailnet(hostName: "phux-rune-ios")
for await snapshot in directory.snapshots() { /* display state and peers */ }
await directory.signOut()

let systemVPNIsOn = TailnetPairing.hasSystemTunnel()
let link = try await TailnetPairing.bootstrap(
    peer: selectedPeer, username: username, password: optionalExplicitPassword,
    onBanner: { checkURL in /* display a user-initiated sign-in button */ }
)
// Pass link directly to the shared Rust pairing parser. Never log it.
```

`TailnetPeer`, `TailnetSnapshot`, their phases and `TailnetDirectory` follow
the phux-mobile reference APIs, without its email-derived SSH username guess:
a tailnet owner is not an operating-system account. Cancel the stream's consuming task to stop
the directory node. State lives in Application Support, excluded from backup;
the SDK disables log uploads before creating a node. Sign-out resets its
authentication, closes it, then deletes its state and ends active streams.
Start a new stream to resume discovery after signing out. No key export is exposed.

`SSHFailure.authenticationFailed(passwordAccepted: true)` lets the app ask for
the computer account password; `false` means the server wants a step this
client can't answer (keyboard-interactive, such as a one-time code, a key, or a
second factor after the password). Authentication always tries `none` first.
`onBanner` receives only a bounded `https://login.tailscale.com/a/...` check
URL for netmap-pinned Tailscale SSH, never arbitrary SSH output. Other failures
implement `LocalizedError` and contain no remote output or pairing secrets.

The embedded node is a directory, not a system VPN. Pairing requires a system
`utun` interface carrying `100.64.0.0/10` or `fd7a:115c:a1e0::/48`. Ordinary
SSH additionally requires the selected MagicDNS name to resolve to the same
netmap IPs; dual-stack answers are supported. Tailscale SSH pins its SSH host
keys from the netmap. Both modes dial the checked IP, never a second DNS lookup.

Pairing only locates existing `phux` and runs `pair --json --enroll --name`
under `umask 077`. It never installs, upgrades, starts or adopts a server.
Failures require the operator to configure an already-running paired server
with reachable QUIC. Script output is bounded and only JSON after the last
marker is consumed. Neither stdout nor stderr is shown to the user or logged.
Returned destinations must match the selected peer's netmap IPs or exact
MagicDNS name. Wildcard listeners are rewritten to the authenticated peer IP.
The final link is canonical HTTPS at `phux.sh/connect`, QUIC-only. Rust still
owns all token, certificate, CA and enrollment validation before persistence.

## Direct build integration

`ios/tailnet.py` exports:

```python
result = prepare(out: Path, env: dict, target: str, release: bool)
```

`target` is `device` or `simulator`. Append `result["swift_flags"]` directly to
the app's `swiftc` command. It includes the static NIO/SSH dependency closure,
Swift modules, C shim module maps, framework search path and app rpath.
Copy `result["frameworks"]` into the app's `Frameworks` directory and sign each
framework before signing the app. Copy `result["resources"]` to the app root,
preserving bundle names. This includes static-package privacy bundles and
the first-party privacy manifest. Append `result["notices"]` to app notices.

All source mirrors, package caches, artifact files, module caches and build
products are under `out/tailnet`. The helper never accesses signing keys or
changes the system VPN. SwiftPM uses the checked-in exact dependency lock and
cannot update it to a new version. The TailscaleKit archive's SHA-256 is checked
before extraction, including when a cached archive is reused; SwiftPM then
links that verified local binary rather than downloading another copy.
`RUNE_TAILSCALE_ARCHIVE` optionally supplies an offline archive with the same
mandatory checksum. No development checkout is used as a dependency.

The package is built by SwiftPM's `swiftbuild` engine with an explicit iOS SDK,
`arm64-apple-ios26.0[-simulator]`, and warnings as errors for `RuneTailnet` and
`RuneTailnetTests` in every configuration. The manifest scopes this policy to
first-party targets; forwarding it globally conflicts with SwiftBuild's
dependency warning suppression on Xcode 26. The direct UIKit compiler retains
its own warnings-as-errors gate. Build commands and
logs are retained under `out/tailnet`, plus `integration.json` with paths and
notices. No worktree `.build` is created.

## Tests and attribution

`python3 clients/rune/scripts/test_ios_tailnet.py` tests the helper without a
network or Apple credentials. Swift package tests cover directory JSON,
tunnel/netmap guards, bounded script parsing, safe POSIX quoting, destination
binding, and real local SSH exchanges: `none`/password authentication, pins,
exec input/output/status, refusal, output limits, deadlines and cancellation.
Run Swift tests using a source mirror and explicit scratch/cache paths just
as the helper does; use a macOS SDK/triple for the host tests. No sign-in or
production phux process is needed.

Directory, status, SSH and system-tunnel primitives are adapted with the
owner's authorization from phux-mobile's PhuxTailnet and OpenCodeTailnet
(Copyright 2026 Phil Hall). `license-sources.json` pins the 30 Go/Tailscale
modules and SHA-256 values of the actual artifact's license texts. The helper
fetches the exact public upstream license texts, verifies each hash, and caches
them under `out/tailnet/licenses`; no private phux-mobile access is needed.
`license_sources.py` reproduces this small metadata file from the pinned
`scripts/licenses/tailscalekit.json` build inventory (its commit and file hash
are recorded in the script). No full generated inventory is vendored or split.
Other Swift package license texts are collected from the exact resolved
dependency checkouts during the build.
