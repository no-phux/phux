---
audience: agents, contributors
stability: stable
last-reviewed: 2026-09-27
---

# phux Project Instructions for Agents

**TL;DR.** phux-specific agent guidance layered on [`AGENTS.md`](./AGENTS.md)
(universal rules): how to build and test (`mise install` or `nix develop`,
then `just ci`), the crate/architecture map, and the project conventions.

Commit verified task changes before handoff per
[AGENTS.md's completion policy](./AGENTS.md#finish-the-work); that policy
overrides the conservative commit defaults in the managed Beads block and
`bd prime`.

## Build & Test

Setup and version details live in [`docs/SETUP.md`](./docs/SETUP.md).

```bash
mise install        # or: nix develop
just doctor native  # or core / docs / integrations / web / cockpit / ci
just core-check     # example scoped loop; see SETUP.md for other areas
just ci             # full root deterministic/unit gate set
just ci-full        # ci + the real-server e2e and agent smoke lanes
just check          # quick type-check
just test           # cargo nextest run --workspace
```

**`just ci-full` is the full root PR bar; scoped gates are the inner loop.**
CI's `test` job also runs `just e2e` and `just agents-fleet-smoke`, which
`just ci` omits, so `just ci` green can still fail a required check. See
CONTRIBUTING.md §"Bar for any change" for the gate-by-gate map. `commitlint`
lints every commit in the PR, not just the title.

## How work reaches `main`

`main` has one ruleset: no deletion, no force push, linear history, pull
request required, required checks `ci` and `commitlint`. Organization admins
and the release App may bypass it; a bypass never replaces `just ci-full` and a
PR with hosted checks, and still lands as a non-force fast-forward.
Cockpit-only diffs skip the Rust compile lanes; see `docs/RELEASING.md` for the
routing matrix.

## Architecture Overview

phux is a **libghostty-backed terminal control plane**. The wire is
asymmetric: server→client *terminal content* is **VT bytes** forwarded
from the PTY ([ADR-0013](./docs/adr/0013-libghostty-bytes-on-wire.md));
client→server *input* is **structured key, mouse, focus, and paste
events** built from libghostty's atoms (ADR-0006, ADR-0008). The
protocol is layered as L1 Terminal substrate + L2 Collection + L3
Metadata ([ADR-0015](./docs/adr/0015-protocol-layering.md)). One server per
user ([ADR-0003](./docs/adr/0003-server-process-model.md)); one tokio
current-thread runtime; UDS transport with a QUIC future
([ADR-0007](./docs/adr/0007-mosh-class-transport-and-satellites.md)).

Authoritative docs, in order of priority:

- [`docs/CONCEPTS.md`](./docs/CONCEPTS.md) — canonical mental model.
- [`docs/CONVENTIONS.md`](./docs/CONVENTIONS.md) — doc system, ADR template.
- [`docs/spec/`](./docs/spec/) — normative wire protocol. Code conforms
  to it, not vice versa.
- [`docs/architecture/`](./docs/architecture/) — internal structure.
- [`docs/consumers/tui.md`](./docs/consumers/tui.md) — TUI consumer surface.
- [`docs/operations.md`](./docs/operations.md) — errors, logging, security.
- [`docs/vision.md`](./docs/vision.md) — the long arc.
- [`docs/adr/`](./docs/adr/) — decisions, with rationale and tradeoffs.

Crates: twenty, all under `crates/*`, all workspace members. The ones
you touch most: `phux-protocol` (wire), `phux-core` (domain),
`phux-agent-rules` (agent manifest evaluator: regions, TOML rules, offline
explain), `phux-server` (daemon), `phux-tui` (the attach driver, libghostty
replicas, and ratatui chrome), `phux-client` (the headless client library
behind the agent verbs and MCP; no `ratatui`), `phux-client-core`
(pane-interior substrate and session kernel; no `ratatui` and no `tokio`,
ADR-0020 and ADR-0100), `phux-client-runtime` (the one client orchestration
layer: registry resolution, dial planning, reconnect policy, the relay tunnel;
ADR-0133), `phux-client-ffi` (the one binding crate: `projection/` plus the
`c-abi` encoder for Cockpit and native embedders and the `uniffi` encoder for
phux-mobile; ADR-0135), `phux-config` (TOML + widgets + the settings
catalogue), `phux` (binary). Every crate has a section in
[`docs/architecture/module-structure.md`](./docs/architecture/module-structure.md)
— read it before assuming a capability is missing. `phux-protocol` is
publishable; the rest are `publish = false`.

## Conventions & Patterns

- **Docs follow [`docs/CONVENTIONS.md`](./docs/CONVENTIONS.md)**: frontmatter,
  `**TL;DR.**`, one fact per home; `just docs-check` enforces it.
- **No emojis in committed files.**
- **Conventional commits.** `feat(scope): ...`, `fix(scope): ...`,
  `docs(scope): ...`, `chore(scope): ...`.
- **`docs/spec/` is normative.** Wire changes update the spec, add a
  `docs/spec/CHANGELOG.md` entry, and follow the versioning rules in
  CONTRIBUTING.md. Wire bytes are owned by `phux-protocol`.
- **ADR for any decision that closes off a design space**; bug fixes don't
  need one.
- **`unsafe` requires a `// SAFETY:` comment.** Library crates default
  to `forbid(unsafe_code)`.
- **No new deps without a paragraph of justification in the PR.**
- **Linear history on `main`.** Rebase, ff-only merges; no `--no-ff`.
- **Never implement on `main` or in the primary repository worktree**; see
  AGENTS.md §"Worktree Isolation". Parallel agents use self-managed worktrees
  (CONTRIBUTING.md §"Multi-agent fan-out").
