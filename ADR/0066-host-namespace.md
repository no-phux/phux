---
audience: contributors
stability: stable
last-reviewed: 2026-08-01
---

# 0066 — One `phux host` namespace over the split machine registries

**TL;DR.** `phux remote`, `phux satellite`, and top-level `phux enroll`
collapse into one visible namespace: `phux host add|ls|rm|enroll` with a
`--role remote|satellite` axis (default `remote`). The old verbs survive one
release as hidden aliases that print a stderr deprecation note. Config stays
split — `[[remote]]` and `[[satellites]]` encode opposite trust directions.
Refines ADR-0038 and ADR-0055 without superseding either.

Status: Accepted
Date: 2026-08-01

## Context

The CLI grew four near-duplicate concept pairs that force users to learn
distinctions the product should absorb:

1. **`phux enroll HOST` vs `phux satellite enroll HOST`**
   (`crates/phux/src/commands/mod.rs:1324-1355` vs `mod.rs:1841-1874`): same
   positional, same five flags (`--name`, `--endpoint`, `--quic-port`,
   `--no-service`, `--ssh-only`), differing only in `--session` vs `--json`.
2. **`phux remote` vs `phux satellite`** (`mod.rs:1431-1472` vs
   `mod.rs:1826-1922`): both are add/list/remove registries of other phux
   servers, both carrying `--token-file` and `--cert-fingerprint`.
3. **`phux pair` vs `phux relay pair`** (`mod.rs:1277` vs
   `crates/phux/src/commands/relay.rs:59`): same verb, unrelated objects — a
   device bearer token vs a relay route token.
4. **`phux workspace` vs `phux worktree`** (`mod.rs:1200` vs `mod.rs:1426`):
   one letter apart, both git-checkout-shaped, with `workspace inspect` and
   `worktree list` overlapping.

Pairs 1 and 2 are the same product concept — "register another machine" —
split by which trust direction the entry encodes. Pairs 3 and 4 are genuinely
different objects that merely collide in name.

## Decision

1. **`phux host` is the one visible namespace for machine registration.**
   Subcommands: `add`, `ls` (visible alias `list`), `rm` (visible alias
   `remove`), `enroll`. `add`, `rm`, and `enroll` take
   `--role remote|satellite`, defaulting to `remote`. `host ls` with no
   `--role` lists both registries with a role column; `--role` filters.
2. **Config stays split. No `[[hosts]]` array, no migration.** `--role
   remote` operates on `[[remote]]`, `--role satellite` on `[[satellites]]`.
   The two schemas stay deliberate siblings
   (`crates/phux-config/src/remote.rs:7-12`): the fields line up because both
   describe "a phux server reachable over a pinned TLS transport," but the
   trust direction is opposite — a satellite is a peer a *hub* dials on
   behalf of its users; a remote is a server *this consumer* dials on behalf
   of itself. Collapsing them would let `phux host enroll` edit federation
   topology by accident.
3. **Deprecation: hidden aliases for one release cycle.** The alias mapping
   is exact:

   | Hidden alias (one release) | Visible replacement |
   |---|---|
   | `phux remote add NAME ENDPOINT` | `phux host add NAME ENDPOINT` |
   | `phux remote list` | `phux host ls` |
   | `phux remote remove NAME` | `phux host rm NAME` |
   | `phux satellite add NAME ENDPOINT` | `phux host add --role satellite NAME ENDPOINT` |
   | `phux satellite list` (alias `ls`) | `phux host ls --role satellite` |
   | `phux satellite remove NAME` (alias `rm`) | `phux host rm --role satellite NAME` |
   | `phux satellite enroll HOST` | `phux host enroll --role satellite HOST` |
   | `phux enroll HOST` | `phux host enroll HOST` |

   Each alias still works, is hidden from `--help`, and prints a one-line
   stderr note naming its replacement. Alias `--json` output moves to the host document schemas immediately (the `"satellites"`/`"satellite"`/`"remotes"` shapes are retired on day one, before the aliases themselves are removed); release notes must flag this as the breaking part of the flip. Shell completions are generated from
   the visible tree only, so the old verbs stop completing immediately.
4. **Enroll flag asymmetry is resolved, not preserved.** `--json` becomes
   valid on both roles — remote-role enroll gains stable JSON output, a
   local, zero-wire-change addition. `--session` stays remote-only:
   under `--role satellite` it is rejected with an error that names the
   remedy (satellite links are hub-dialed; there is no arrival to attach).
5. **`pair`, `relay pair`, `workspace`, and `worktree` keep their names.**
   They are different objects, not one concept split in two. They get
   cross-referencing help text that names the neighbor and the distinction.
6. **Adjacent fixes ride along** (same release, separate changes): the
   machine-only `stdio-bridge` verb is hidden from `--help`
   (`mod.rs:1240-1245`); the attach name-shadowing rule — a `[[remote]]`
   registry name wins over a same-named local session
   (`crates/phux/src/commands/attach.rs:539-547`) — is documented in
   `attach --help`; the `agent` subcommand family gets doc comments on its
   args (`crates/phux/src/commands/agent/mod.rs:26-90`).
   `agent install-claude` / `uninstall-claude` keep their names.

## Why

- **One mental model.** "Register a machine" is one user intention; the CLI
  should absorb the trust-direction split into a flag with a safe default,
  not fork the verb tree. The five-shared-flag overlap between the two
  enrolls is evidence they were always one verb.
- **Config split is load-bearing; CLI split is not.** The schema comment in
  `remote.rs:7-12` records why the *storage* must stay split. Nothing there
  requires two *verb trees* — a `--role` axis preserves the boundary while
  presenting one surface.
- **Hidden-alias deprecation is the established pattern.** Renames are
  breaking; one release of hidden aliases with a stderr note gives scripts a
  migration window without advertising the old names to new users.
- **Refines, does not supersede.** ADR-0038 (hub-satellite auth) and
  ADR-0055 (always-on server and ssh-bootstrapped enrollment) decided trust
  and bootstrap semantics; this ADR only renames the surface those semantics
  are reached through. Their invariants are untouched.

## Tradeoffs

- **`--role` is a mode flag on a destructive verb.** `host rm NAME`
  defaulting to remote means removing a satellite requires remembering the
  flag; forgetting it errors on a name miss rather than deleting the wrong
  entry, since the registries are disjoint stores.
- **One release of doubled surface.** Aliases keep parse arms and dispatch
  paths alive; the cost is bounded by the removal deadline.
- **`host` now competes with `--host` flags** (e.g. `pair --host`) for the
  reader's attention. Accepted: the noun is right, and the flag is scoped.
- **Scripts break at alias removal.** Deliberate — the stderr note runs for
  a full release first.

## Alternatives

- **Unified `[[hosts]]` config with a `role` field.** Rejected: collapses
  the trust boundary the schema comment exists to defend — a single
  enrollment path could then rewrite federation topology
  (`remote.rs:7-12`).
- **Rename only (`satellite` → something less remote-like), keep three verb
  trees.** Rejected: leaves the enroll duplication and the shared-flag
  drift; the pairs would diverge again.
- **Also merge `pair`/`relay pair` or `workspace`/`worktree`.** Rejected:
  those name different objects; merging would create real ambiguity to fix
  spelled-out ambiguity. Help cross-references are the right-sized fix.
- **Do nothing.** Rejected: four documented confusion pairs, two of which
  are the same concept, is a learnability tax on every new operator.
