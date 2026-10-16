---
audience: contributors
stability: stable
last-reviewed: 2026-10-16
---

# 0175 — One release manifest per channel, and Desktop updates on restart

**TL;DR.** CI publishes one JSON manifest per channel at a fixed URL,
`releases/download/channels/{stable,next}.json`, derived from what is
actually published. It names every product's version, commit, protocol, and
per-target artifact with its SHA-256. `phux update`, the shell installers,
and Phux Desktop read only that file; none of them lists GitHub releases.
Desktop downloads and verifies an update into a staging bundle while it runs
and swaps it in only after the app has exited.

Status: Accepted
Date: 2026-10-16
Amends [ADR-0074](./0074-self-update-trust-boundary.md) decision 1 and
supersedes in part [ADR-0113](./0113-next-release-channel.md) decisions 3
and 5 (clients no longer read `channel.json`).

## Context

Three implementations answered "what is the newest release": the Rust CLI
(the `releases/latest` redirect, then a 30-row API list), the Rust desktop (up
to ten 100-row API pages), and the shell installers (a 250-line awk JSON
scanner over up to ten 30-row pages). All three called `api.github.com`
unauthenticated, at 60 requests per hour per address, against one repository
that publishes CLI, desktop, and plugin releases about ten times a day. When
the budget ran out, checks failed and pages showed stale results. Only the
CLI could use `releases/latest`, and only because it publishes last. Next had
per-product pointers; stable had none.

Desktop installed by running the installer from inside the app. The installer
renamed the running bundle aside and deleted it on exit, while the app still
ran from it, then asked the app to quit. The install had no timeout.

## Decision

1. **The manifest is the only discovery input.** For each channel `C`,
   `https://github.com/no-phux/phux/releases/download/channels/C.json` is the
   one document clients fetch. The URL is a constant; `C` is the closed set
   `stable`, `next`. The `channels` prerelease holds only these files and is
   never GitHub's Latest.
2. **Schema 1.** Pretty-printed with two-space indentation, one member per
   line, no arrays, and string values restricted to `[A-Za-z0-9._:/+-]`, so
   POSIX `sh` reads it with a small awk script and no JSON library:

   ```json
   {
     "schema_version": 1,
     "channel": "stable",
     "generated_at": "2026-10-16T20:00:00Z",
     "products": {
       "phux": {
         "version": "0.85.1", "release": "v0.85.1", "sha": "<40 hex>",
         "protocol": "0.9",
         "artifacts": {
           "aarch64-apple-darwin": {
             "name": "phux-v0.85.1-aarch64-apple-darwin.tar.gz",
             "url": "https://github.com/no-phux/phux/releases/download/v0.85.1/phux-v0.85.1-aarch64-apple-darwin.tar.gz",
             "sha256": "<64 hex>"
           }
         }
       },
       "desktop": {
         "version": "0.24.0", "release": "desktop-v0.24.0", "sha": "<40 hex>",
         "protocol": "0.9", "minimum_macos": "27.0",
         "artifacts": { "aarch64-apple-darwin": { "name": "phux-desktop-0.24.0-macos-arm64.zip", "url": "...", "sha256": "..." } }
       }
     }
   }
   ```

   (The real file has one member per line.) `phux` artifacts cover
   `aarch64-apple-darwin`, `x86_64-unknown-linux-gnu`, and
   `aarch64-unknown-linux-gnu`. On `next`, `release` is `next` and names are
   `phux-next.<sha>-<target>.tar.gz` and
   `phux-desktop-next.<sha>-macos-arm64.zip`. A product never published on a
   channel is absent. Additive members do not bump `schema_version`.
3. **Clients validate it before using it.** The schema version and channel
   must match. Versions must be strict `X.Y.Z`, SHAs 40 lowercase hex, and
   digests 64 lowercase hex. Each client derives the artifact `name`,
   `release`, and `url` itself from the version or SHA and the host target,
   and refuses a manifest that disagrees. A network string never steers a
   download, as before.
4. **The manifest digest is the trust anchor for its targets.** A client
   verifies downloaded bytes against the manifest's `sha256` before
   unpacking. An explicit pin (`--version`, `--revision`) bypasses the
   manifest and is verified against that release's published sidecar. Both
   files come from the same host over the same TLS channel, so this is the
   same trust ADR-0074 states. Signing one manifest is now the single place
   to add an independent root.
5. **The manifest is generated from what is published.**
   `scripts/ci/release_manifest.py` builds `stable.json` from the newest
   published, non-draft `vX.Y.Z` and `desktop-vX.Y.Z` releases (authenticated
   API, sidecars, tag source). It builds `next.json` from the next
   prerelease's per-product pointers and sidecars. It uploads only when the
   content changed. `publish.yml` regenerates `stable.json` on every run,
   including the daily reconcile. `next-release.yml` regenerates `next.json`
   on every gated run, so both converge without a read-modify-write. A
   product that cannot be resolved keeps its published entry and fails the
   job; a read error is never taken for "not published".
6. **Pre-manifest clients keep working.** CLI releases stay GitHub's Latest,
   and `next/channel.json` plus `next/desktop-channel.json` stay as the
   per-product publication records `next.json` is built from. Binaries
   released before this ADR update through those records once and read the
   manifest from then on.
7. **Desktop stages while running and swaps after exit.** *Download update*
   runs the installer with `--stage-dir <apps>/.phux-desktop-update
   --sha256 <manifest digest>` under a timeout that kills the process group.
   The installer downloads, verifies, unpacks, checks the signature and
   plist identity, and leaves `Phux.app` there. *Restart to update* quits the
   app. Quitting with a staged update also installs it. A detached helper
   waits until no process runs from the installed bundle, takes the
   installer's `lockf` lock, checks again,
   renames the old bundle aside, renames the staged one into place (moving
   the old one back on failure), and relaunches when asked. It logs to
   `~/Library/Logs/Phux/update.log`. Nothing is ever deleted from under a
   running process.
8. **Protocol travels with the release.** Each product's `protocol` is the
   `major.minor` it speaks (ADR-0061). `phux update --json` reports the
   current and target protocol. When an update changes the protocol, the
   Desktop Updates page says that phux must move too.

## Why

A static file at a fixed URL is one cheap request with no rate limit and no
pagination. Release order and stream mixing stop being client problems.
Generating the file from published state means a crashed or cancelled run
leaves the previous manifest correct, and the next run repairs it. Pretty,
array-free JSON is what lets `curl | sh` stay dependency-free without a
second format to drift. Staging, then swapping after exit, is how Sparkle,
Chrome, and VS Code update; the running image never loses its files.

## Tradeoffs

There is a short window during `--clobber` where a manifest is missing. A
client reports a failed check and the next check succeeds. Another moving
release appears in the release list. Desktop's staged bundle occupies a
hidden directory beside the app until it is applied; the app removes a stale
one at launch when it can take the lock. Pinning bypasses the manifest, so it
keeps the sidecar code path.
