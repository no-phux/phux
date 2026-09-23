---
audience: contributors
stability: stable
last-reviewed: 2026-09-23
---

# 0138 — Cockpit rides the next channel, and next queues instead of cancelling

**TL;DR.** The moving `next` prerelease now carries Phux Cockpit beside the
CLI: `phux-cockpit-next.<sha>-macos-arm64.zip`, a `.sha256` sidecar, and its
own `cockpit-channel.json` pointer. A bundle bakes its channel into
`Info.plist`, so the in-app updater follows it. `phux update` and
`phux channel` move an installed Cockpit onto the CLI's channel. The
workflow queues a newer green SHA behind the build in flight instead of
cancelling it.

Status: Accepted
Date: 2026-09-23

## Context

ADR-0113 gave the CLI a `next` rail and excluded Cockpit. Trying Cockpit from
`main` still meant cutting a `cockpit-vX.Y.Z` release, which is the cost that
ADR-0113 removed for the CLI. The app embeds the FFI and the coordinator CLI,
so most Rust changes are Cockpit changes too.

ADR-0113's cancel-in-progress group also starves on a busy `main`. A
three-target build takes about ten minutes, and merges that land more often
than that cancel every build. In the 100 runs before this ADR, 14 builds were
cancelled and only 11 published.

## Decision

1. **One prerelease, two independent products.** `next-release.yml` decides
   each product separately: the CLI against the SHA in `channel.json`, and
   Cockpit against the SHA in `cockpit-channel.json` using classify-changes'
   `cockpit_needed`. A product publishes on its own success, and pruning
   touches only that product's assets. A failed Cockpit build never holds
   back the CLI.
2. **Cockpit next builds are ad-hoc signed.** They are built by
   `package-macos.sh` with `PHUX_BUILD_CHANNEL=next` and `PHUX_BUILD_SHA`,
   which write `PhuxChannel` and `PhuxBuildSHA` into `Info.plist`. The build
   has no soak, no DMG upload, no Homebrew change, and no notarization.
   Stable bundles carry neither key.
3. **The channel lives in the bundle.** `cockpit-self-update.sh` follows
   `PhuxChannel` when no channel is given. On next it compares SHAs; on
   stable it compares versions. Moving to a channel other than the bundle's
   is always an install. `install-cockpit.sh --channel next` resolves the
   pointer and verifies the sidecar before unpacking, the same gate stable
   uses.
4. **The CLI drives Cockpit.** On macOS, `phux update` (and so
   `phux channel`) runs that driver against the installed app on the CLI's
   resolved channel. `--check` only reports, and `--dry-run` and
   `--rollback` leave the app alone. The binary embeds both scripts, so an
   app built before this ADR can still be switched. The driver's refusals
   (Homebrew, Nix, dev builds) still apply. The JSON document gains
   `cockpit` and `path_shadowed_by`, both additive.
5. **Queue, don't cancel.** The concurrency group keeps
   `cancel-in-progress: false`. GitHub keeps one run in flight and replaces
   any pending run with the newest one, so a burst of merges still collapses
   into one follow-up build.

## Why

The bundle is the only state that cannot drift from what is installed. A
separate preference file could claim `next` over a stable app. The CLI
embeds the scripts so there is still one download stack while old bundles
still upgrade. Queueing trades up to one build of latency for a guarantee
that `next` eventually reaches the tip.

## Tradeoffs

Cockpit on `next` is ad-hoc signed, as stable is today, so macOS privacy
grants may be asked for again after an update. Each Cockpit build takes one
macOS runner for about 15 minutes. `phux update` now touches a second
product. A user who wants only the CLI to move can uninstall Cockpit or
install it with Homebrew, which the driver refuses.

## Alternatives

**A separate `cockpit-next` prerelease.** This means a second moving tag and
a second pruning story for no benefit. The two pointers already keep the
products independent.

**Build Cockpit in the CLI's matrix and share one pointer.** A Cockpit
failure would then block the CLI rail, and a Cockpit-only change would
rebuild three Linux and macOS targets.

**Keep cancel-in-progress and add a debounce.** A debounce still starves
under steady merges, and it adds latency when `main` is quiet.

## Related

- ADR-0113 — the next channel this extends.
- ADR-0074 — the checksum-before-unpack trust boundary, unchanged.
