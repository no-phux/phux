---
audience: contributors
stability: stable
last-reviewed: 2026-10-16
---

# 0176 — The release pipeline

**TL;DR.** Two products (`phux`, `desktop`) ship on two channels (`stable`,
`next`). Release names follow one table every implementation derives from.
The channel manifest carries only what a client cannot derive: version, commit,
protocol, and one SHA-256 per target, read from GitHub's own asset digests.
One installer installs both products. Release Please cuts stable drafts,
`publish.yml` publishes them on green `ci`, and `next-release.yml` publishes
green `main`.

Status: Accepted
Date: 2026-10-16

## Context

[ADR-0175](./0175-one-release-manifest-per-channel.md) gave every client one
discovery file. Its first shape repeated each asset's name, release, and URL,
and each client then derived those itself and rejected any mismatch. That
meant one derivation in each of four languages, plus three cross-checks. The
generator rebuilt digests by downloading sidecars, and it rebuilt next from
per-product pointer files. Two installers shared a resolver by pasting it into
each other. Both release workflows carried the same three-target build. The
Homebrew formula had two writers, and the release decisions were spread over
ADR-0113, ADR-0175, and an 816-line runbook.

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
2. **The manifest names what cannot be derived.**
   `releases/download/channels/<channel>.json`, schema 1:

   ```json
   {
     "schema_version": 1,
     "channel": "stable",
     "generated_at": "2026-10-16T20:00:00Z",
     "products": {
       "phux": {
         "version": "0.86.1",
         "sha": "<40 hex>",
         "protocol": "0.9",
         "sha256": { "aarch64-apple-darwin": "<64 hex>" }
       }
     }
   }
   ```

   A product that was never published on a channel is absent. The file is
   pretty-printed, one member per line, with no arrays, and string values
   restricted to `[A-Za-z0-9._:/+-]`, so POSIX `sh` reads it with one awk
   pass. Additive members do not bump the schema.
3. **Clients validate, then derive.** The schema and channel must match.
   Versions must be strict `X.Y.Z`, commits 40 lowercase hex, and digests 64
   lowercase hex. Names and URLs come from decision 1 alone, so a network
   string never steers a download. The digest verifies the bytes before
   anything is unpacked. A pin (`--version` for stable, `--revision` for next)
   skips the manifest and verifies against the sidecar. This is the trust
   that [ADR-0074](./0074-self-update-trust-boundary.md) states, with the
   manifest as its anchor.
4. **The manifest is generated from published state.**
   `scripts/ci/release_manifest.py` reads GitHub's `digest` for each derived
   asset name, so it downloads nothing. Stable takes the newest published
   `vX.Y.Z` and `desktop-vX.Y.Z`, and `publish.yml` rebuilds it on every run.
   Next is written by the same script when a product's build lands on the
   `next` prerelease. The publisher replaces that product's entry and keeps
   the other one. `next-release.yml` runs one at a time, so the
   read-modify-write cannot race. A product that cannot be resolved keeps its
   published entry and fails the job.
5. **Stable.** Release Please owns versions, tags, and draft notes. A draft
   publishes only when `ci` is green for its tagged commit. `publish.yml` is
   the only door. It builds, attaches every asset, publishes, rebuilds
   `stable.json`, and runs the daily drift check. CLI releases are GitHub's
   Latest, and the Homebrew tap renders its formula from Latest on its own
   schedule. Desktop and integration releases never are Latest.
6. **Next.** A moving prerelease of green `main`. Each product rebuilds only
   when its inputs changed since the commit `next.json` names. Runs queue
   instead of cancelling, so a busy `main` still ships its tip. Each product
   keeps its current and previous commit's assets.
7. **One installer.** `scripts/install.sh` installs the CLI. With `--desktop`
   it also installs Phux Desktop, and with `--stage-dir` it only stages the
   app for Desktop's restart-to-update
   ([ADR-0175](./0175-one-release-manifest-per-channel.md) decision 7).
   `phux.sh/install-desktop` is a shim that runs it with `--desktop`.
8. **Pre-manifest binaries are bridged by records nothing current reads:**
   `next/channel.json`, `next/desktop-channel.json`, stable desktop
   `SHA256SUMS`, and the CLI's Latest flag. The publisher writes them; they
   go once no supported install predates the manifest.

## Why

A client that derives every name needs only the facts it cannot compute. Each
extra field is one more thing to check and one more way to disagree. GitHub
computes asset digests on upload, so the publisher records them instead of
recomputing them. One table, one installer, one build workflow, and one
generator leave a single place to change each rule.

## Tradeoffs

The manifest is no longer self-describing for a reader that does not know
decision 1. Dropping the tag retarget leaves the `next` git tag at its first
commit, which matters only to someone reading the tag. Homebrew can trail a
stable release by up to fifteen minutes. A `--clobber` upload leaves a short
window with no manifest, and the next check succeeds.
