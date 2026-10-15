---
audience: contributors, agents
stability: stable
last-reviewed: 2026-10-14
---

# 0167 — The Zig Cockpit is frozen

**TL;DR.** The Zig Cockpit stays in the tree and is not maintained. The
GPUI desktop is the native client. Automatic Cockpit CI, next-channel
builds, and release-please cuts stop. A later sprint can turn those
back on without reconstructing the app.

Status: Superseded by ADR-0169
Date: 2026-10-14

## Context

Cockpit is a Zig macOS app over `phux-client-ffi`. Its pull-request
workflow compiles Zig and the same-checkout FFI whenever a change
touches the FFI or CLI closure, and `next` rebuilds the app when those
inputs move. That cost remained after the desktop became a GPUI app
([ADR-0165](./0165-gpui-desktop-replaces-the-solid-host.md)).

The source is still a complete client. Deleting it would throw away a
resume that is a trigger change, not a rewrite.

## Decision

Freeze `clients/cockpit` at the version in `clients/cockpit/version.txt`.

- The source, local `just cockpit-*` recipes, installer, and release
  workflows stay.
- `cockpit-ci.yml` runs only on `workflow_dispatch`.
- `next-release.yml` forces `cockpit_build=false` and does not publish
  a new Cockpit next archive.
- release-please no longer versions `clients/cockpit`.
- `publish.yml` can still publish an existing `cockpit-vX.Y.Z` tag.
- The maintained native client is the GPUI desktop.

Resume steps live in `clients/cockpit/FROZEN.md`. The consumer guide
states the freeze.

## Consequences

- Pull requests and green `main` no longer spend a macOS runner on Zig
  Cockpit.
- Installed copies and the last published release stay installable.
  `phux update` still follows
  [ADR-0138](./0138-cockpit-rides-the-next-channel.md) when a next
  archive exists; this decision stops producing new ones.
- FFI, the C ABI, and mobile bindings are unchanged. They are not
  Cockpit-only.
- A resume sprint re-enables the workflow triggers, the next-channel
  gate, and the release-please package. It does not require a new
  client.

## Alternatives

- Delete `clients/cockpit`. Resume becomes archaeology, and the
  installer and docs lose their subject.
- Keep automatic CI and only stop feature work. The runner cost and
  the release train remain.
