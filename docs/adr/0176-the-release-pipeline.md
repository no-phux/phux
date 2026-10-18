---
audience: contributors
stability: stable
last-reviewed: 2026-10-16
---

# 0176 — The release pipeline

**TL;DR.** Two products (`phux`, `desktop`) ship on two channels (`stable`,
`next`). Release names follow one table every implementation derives from.
Clients read four facts from the channel manifest: version, commit,
protocol, and their target's SHA-256. The publisher takes those digests
from GitHub's own asset records. One installer installs both products.
Release Please cuts stable drafts, `publish.yml` publishes them on green
`ci`, and `next-release.yml` publishes green `main`.

Status: Accepted
Date: 2026-10-16
Superseded in part by [ADR-0178](./0178-release-checksums-are-minisign-signed.md): decision 3; the signed sidecar is the trust root, and the manifest digest must agree with it.

## Context

[ADR-0175](./0175-one-release-manifest-per-channel.md) gave every client one
discovery file. Its entries repeat each asset's name, release, and URL. Each
client derived those again and refused any mismatch, so there was one
derivation in each of four languages plus three cross-checks. The CLI and
Desktop each had a private copy of the manifest model. The generator
recomputed digests by downloading sidecars, and it rebuilt next from
per-product pointer files. The two installers shared a resolver by pasting
it into each other. Both release workflows carried the same three-target
build. The Homebrew formula had two writers, and the release decisions were
spread across ADR-0113, ADR-0175, and an 816-line runbook.

## Decision

1. **Products, channels, names.** A release is named by (product, channel,
   version, commit, target):

   | product | channel | release | asset |
   |---|---|---|---|
   | phux | stable | `vV` | `phux-vV-<target>.tar.gz` |
   | phux | next | `next` | `phux-next.<sha>-<target>.tar.gz` |
   | desktop | stable | `desktop-vV` | `phux-desktop-V-macos-arm64.zip` |
   | desktop | next | `next` | `phux-desktop-next.<sha>-macos-arm64.zip` |

   The URL is `releases/download/<release>/<asset>`, and every asset has a
   `<asset>.sha256` sidecar. `phux` targets are `aarch64-apple-darwin`,
   `x86_64-unknown-linux-gnu`, and `aarch64-unknown-linux-gnu`. Desktop is
   `aarch64-apple-darwin`.
2. **Clients read four facts.** The manifest keeps ADR-0175's schema 1 at
   `releases/download/channels/<channel>.json`. Clients read only
   `products.<p>.version`, `.sha`, `.protocol`, and
   `.artifacts.<target>.sha256`. They validate the schema, the channel, and
   each fact's shape (`X.Y.Z`, 40 hex, `M.m`, 64 hex), then derive every name
   and URL from decision 1. A network string never steers a download, and a
   derived member is not cross-checked. `phux_client_runtime::release` is the
   model the CLI and Desktop share. `scripts/install.sh` flattens the file
   with one awk pass.
3. **Trust.** The digest verifies the bytes before anything is unpacked. A pin
   (`--version` on stable, `--revision` on next) skips the manifest and
   verifies against the sidecar. This is the trust
   [ADR-0074](./0074-self-update-trust-boundary.md) states, with the manifest
   as its anchor.
4. **The manifest is generated from published state.**
   `tools/src/release/release-manifest.ts` reads GitHub's `digest` for each
   derived asset name, so it downloads nothing. Stable takes the newest published
   `vX.Y.Z` and `desktop-vX.Y.Z`, and `publish.yml` rebuilds it on every run.
   For next, the same script runs when a product's build lands on the `next`
   prerelease: it replaces that product's entry and keeps the other one.
   `next-release.yml` runs one at a time, so the read-modify-write cannot
   race. A product that cannot be resolved keeps its published entry and
   fails the job.
5. **Stable.** Release Please owns versions, tags, and draft notes. A draft
   publishes only when `ci` is green for its tagged commit. `publish.yml` is
   the only door: it builds, attaches every asset, publishes, rebuilds
   `stable.json`, and runs the daily drift check. CLI releases are GitHub's
   Latest, and the Homebrew tap renders its formula from Latest on its own
   schedule. Desktop and integration releases are never Latest.
6. **Next.** A moving prerelease of green `main`. Each product rebuilds only
   when its inputs changed since the commit `next.json` names. Runs queue
   instead of cancelling, so a busy `main` still ships its tip. Each product
   keeps its current and previous commit's assets.
7. **One installer.** `scripts/install.sh` installs the CLI. `--desktop` also
   installs Phux Desktop, and `--stage-dir` only stages it for Desktop's
   restart-to-update ([ADR-0175](./0175-one-release-manifest-per-channel.md)
   decision 7). `phux.sh/install-desktop` is a shim that runs it with
   `--desktop`.
8. **Older binaries are bridged by what current clients ignore.** The
   manifest's derived members are for v0.87.0 and Desktop 0.26.0, which
   validate them: `release`, `artifacts.<target>.name` and `.url`, and
   `minimum_macos`. The pre-manifest bridges are `next/channel.json`,
   `next/desktop-channel.json`, stable desktop `SHA256SUMS`, and the CLI's
   Latest flag. The publisher writes all of them. A bridge goes once no
   supported install reads it. Dropping a manifest member is a schema bump.

## Why

A client that derives every name needs only the facts it cannot compute. A
check against a value it computed itself proves nothing. GitHub computes
asset digests on upload, so the publisher records them instead of
recomputing them. A published schema stays fixed for the binaries that read
it. Simplifying it means reading less of it, not breaking them. One table,
one model, one installer, one build workflow, and one generator leave a
single place to change each rule.

## Tradeoffs

Each entry carries members only older clients read, until those clients age
out. Dropping the tag retarget leaves the `next` git tag at its first
commit, which matters only to someone reading the tag. Homebrew can trail a
stable release by up to fifteen minutes. A `--clobber` upload leaves a short
window with no manifest, and the next check succeeds.
