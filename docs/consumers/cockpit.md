---
audience: humans, consumers, contributors, agents
stability: stable
last-reviewed: 2026-10-14
---

# Zig Cockpit archive

**TL;DR.** The Zig Cockpit is tabled and removed from main, not lost.
A published branch and annotated tag retain the complete app and its
integration hooks. The GPUI desktop is the maintained native client.

## Recovery refs

| Ref | Purpose |
|---|---|
| `archive/zig-cockpit` branch | Preserved source tree, tests, SDK pins, build scripts, installers, and CI/release hooks |
| `archive/zig-cockpit-2026-10-14` annotated tag | Immutable pre-removal snapshot at `2d267e7a9` |

Both refs are published in `no-phux/phux`. The snapshot includes
`clients/cockpit/FROZEN.md`, which records the earlier freeze and how its
automation was disabled. No new Cockpit releases or installs are supported
from `main`; existing published releases and users' installed copies are
not deleted or modified. Use the [GPUI desktop](./desktop.md) for maintained
native development. [ADR-0169](../adr/0169-zig-cockpit-lives-in-an-archive.md)
records the decision.

## Open the preserved app

Run from any clone of this repository:

```sh
git fetch origin refs/heads/archive/zig-cockpit:refs/remotes/origin/archive/zig-cockpit
git fetch origin tag archive/zig-cockpit-2026-10-14
git worktree add -b revive/zig-cockpit ../phux-zig-cockpit archive/zig-cockpit-2026-10-14
```

In that worktree, read `clients/cockpit/README.md`, `FROZEN.md`, and
`docs/SETUP.md` before running `just doctor cockpit`, `just cockpit-test`,
or `just cockpit-build`. Its SDK pin and Rust FFI belong to that snapshot.
Never point a development build at the production socket or copy it over
the installed `phux`.

## Reintegrate with bots

Start a new branch/worktree from current `origin/main`, not from the old
archive. Merging the archive branch alone does not restore deleted files:
its tip is already an ancestor of `main`.

1. Find the dedicated removal commit with
   `git log origin/main --diff-filter=D --format='%H %s' -- clients/cockpit/build.zig`.
2. Revert that commit on the revival branch to recover the app **and all
   integration hooks together**. Resolve conflicts against today's shared
   code; do not overwrite current runtime, FFI, desktop, or release fixes.
   For a source-only experiment, instead restore `clients/cockpit` from the
   snapshot tag and deliberately port the hooks you need.
3. Have bots trace the current protocol and C ABI against the recovered
   provider, rebuild same-checkout FFI and fixtures, and adapt the SDK pin,
   CLI, installer, site, classifier, and release contracts together.
4. Run the recovered Cockpit tests, no-Phux graph, shipping build, C ABI
   checks, and actual isolated app/remote acceptance, plus current root,
   desktop, and browser gates affected by shared changes. A compile is not
   interactive or remote acceptance.
5. Request an independent review. Re-enable automatic CI, next publication,
   release-please versioning, and site promotion only after an explicit
   revival decision and passing evidence. The archived freeze guide is a
   historical starting point, not authority to publish.

Shared `phux-client-ffi`, mobile bindings, the GPUI desktop, and Zig used to
build libghostty remain on `main`. They are not part of the cull.

## Thanks

A big thank-you to **Chris Tate** for the work that made this native-client
experiment possible. The archive preserves that work for another day.
