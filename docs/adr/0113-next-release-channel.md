---
audience: contributors
stability: stable
last-reviewed: 2026-09-12
---

# 0113 — Opt-in next channel from green main

**TL;DR.** Stable stays `vX.Y.Z` via release-please and Homebrew. `next` is a
moving GitHub prerelease of the latest green `main`, published only after CI
succeeds and coalesced so a burst of merges is one build. `phux update
--channel next` follows it. Checksum-before-unpack is unchanged.

Status: Accepted
Date: 2026-09-12

## Context

`phux update` only understands `vX.Y.Z` and `releases/latest`. Getting bits
from `main` meant merging the release-please PR, which rebuilds three targets
from scratch and moves Homebrew. That is the wrong shape for dogfooding:
users who want `@next` should opt in, and stable users should not move.

Public standard runners are free; the scarce limits are five macOS slots and
the 10 GB Actions cache. Per-commit rebuilds and per-SHA GitHub Releases both
lose.

## Decision

1. **Two channels.** `stable` is `vX.Y.Z`, release-please, Homebrew, and
   `releases/latest`. `next` is a prerelease tagged `next`. GitHub's latest
   redirect ignores prereleases, so stable installs cannot drift onto it.
2. **Publish next after green `main` CI**, only when classify-changes says
   the Rust product moved. One concurrency group, cancel-in-progress. Docs-
   only commits produce nothing. No Cargo bump, no Homebrew, no Cockpit.
3. **Same tarball contract as ADR-0074.** Assets are
   `phux-next.<sha>-<target>.tar.gz` plus a `.sha256` sidecar and the six
   members stable already ships. `channel.json` is uploaded last and names
   the SHA; `phux update` derives URLs from that SHA itself and never
   interpolates a network-supplied tag into a download path.
4. **Keep the previous SHA's assets** so an in-flight download cannot 404;
   prune older than two. Identity is the git SHA, not semver: `main` still
   carries the last stable Cargo version.
5. **`phux update --channel next`** follows `channel.json`. The choice is
   persisted in `<bindir>/.phux-channel`. `--version` remains a stable tag.
   Homebrew, Cargo, and Nix installs are still refused.

## Why

A moving prerelease is one GitHub Release, constant storage, and a closed
vocabulary word (`next`) that a hostile redirect cannot steer. Waiting for
CI and coalescing is what keeps this off the macOS slot budget that Cockpit
and PR CI already share. Reusing the tarball layout means the updater's
checksum gate does not grow a second trust story.

## Tradeoffs

`next` can be broken; that is the point of opting in. A moving git tag is a
footgun for anyone who checks it out as a branch. `--check` fetches
`channel.json` (an index, not an archive). Switching back to stable with the
same Cargo version still replaces the binaries, because the channel changed.

## Alternatives

**Mint a semver every day.** Already happening, and it is the expensive
path: from-scratch builds plus Homebrew for people who only wanted bits.

**Homebrew `--HEAD` as the next rail.** `phux update` does not own Cellar
installs (ADR-0074). Next is a direct-release channel.

**cargo-dist / a second build system.** `release.yml` already owns portable
binaries. A parallel packager would drift.

**Per-commit immutable releases.** Storage and release-list noise. Two
generations of SHA-qualified assets on one prerelease is enough.
