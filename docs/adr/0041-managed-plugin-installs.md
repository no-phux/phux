---
audience: contributors
stability: stable
last-reviewed: 2026-07-09
---

# 0041 — Managed plugin installs: snapshot fetches, system tools, one lockfile

**TL;DR.** `phux plugin install` fetches a plugin package — git URL, local
directory, or tarball — into one managed directory under the XDG data dir
using the system `git`/`tar`, runs the manifest's `[[build]]` steps with a
bounded timeout, validates (including `min_phux_version`), and links via
the existing registry path. Provenance lives in a single `plugins.lock`;
`phux plugin update` re-fetches and swaps rather than mutating in place.

Status: Accepted
Date: 2026-07-09

## Context

Wave-1 gave plugins a full declarative surface (manifest entrypoints, an
event dispatcher, palette/keybinding contributions) but acquiring a plugin
was manual: obtain the tree yourself, then `phux plugin link` its manifest.
That leaves no story for "install this plugin from its repo", no record of
where a linked tree came from, and no way to refresh it. The wire is not
involved — plugins are a client-local config concern — so the design space
is purely filesystem layout, fetch mechanics, and provenance.

## Decision

- **One managed directory.** Installed packages live under
  `$XDG_DATA_HOME/phux/plugins` (else `~/.local/share/phux/plugins`), one
  subdirectory per plugin id. Config (`config.toml`) keeps linking by
  manifest path exactly as before; an installed plugin is just a linked
  plugin whose tree phux owns.
- **System tools, no new dependencies.** Git sources are shallow-cloned
  with the system `git`; tarballs are extracted with the system `tar`;
  directories are copied in-process. No git2/libgit2, no tar/flate2 crates
  enter the workspace.
- **Installs are snapshots.** The managed copy carries no `.git` and is
  never mutated in place. `phux plugin update` re-fetches from the
  recorded source into a staging directory, rebuilds, revalidates, and
  atomically swaps the install directory (keeping a backup until the swap
  lands). Update and install are therefore the same code path.
- **One lockfile.** `plugins.lock` at the managed directory root records,
  per plugin id: source kind (`git`/`dir`/`tarball`), the original ref
  (URL or absolute path), the requested branch/tag, and the resolved
  commit for git sources. It is provenance for `update`, not a config
  layer — `config.toml` remains the only linking authority.
- **Build at install time, bounded.** The manifest's `[[build]]` argv
  entries for the current platform run as child processes from the plugin
  root through the phux-plugin runner (captured output, kill-on-timeout,
  five minutes per step). A failing build aborts before anything is
  linked or locked.

## Why

Shelling out to `git` and `tar` keeps the dependency budget at zero and
inherits the user's transports, credentials, and proxy config for free —
matching the repo's standing preference. Snapshot-and-swap semantics make
update failure-safe (the old tree survives until the new one is built and
validated) and make the three source kinds symmetrical; an in-place
`git pull` model would work only for git and can strand a half-updated
tree behind a failed build. A single lockfile mirrors the familiar
package-manager shape, diffs cleanly, and gives `update` everything it
needs without re-asking the user.

## Tradeoffs

- Re-fetching on every update costs bandwidth that an incremental
  `git pull` would save; shallow clones bound the cost.
- Requiring `git`/`tar` on PATH means a bare environment cannot install
  from those sources (directory installs still work). The error names the
  missing tool.
- `dir` and `tarball` sources update only while the original path still
  exists; the lockfile records absolute paths, so moving the source
  requires a reinstall.
- Symlinks in directory sources are refused rather than resolved, so a
  build layout relying on them must ship a tarball or repo instead.

## Alternatives

**A git library (git2) or tar/flate2 crates.** Rejected: three heavyweight
dependencies to replicate tools every target system already has, plus a
second credential/proxy configuration surface to support.

**In-place `git pull` updates.** Rejected: git-only, leaves a mutable
checkout (with `.git`) as the executing artifact, and a failed build after
a pull strands a broken install with no rollback.

**Per-plugin lockfiles inside each install.** Rejected: provenance would
be deleted by the swap it exists to describe; a single sibling lockfile
survives updates and reads as one inventory.

**Recording installs in `config.toml` instead of a lockfile.** Rejected:
resolved commits are machine bookkeeping, not user intent; writing them
into the user's config invites merge noise and hand-edits.
