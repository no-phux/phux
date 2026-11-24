---
group: internals
order: 7
---

# CI

What `ci.yml` runs for each event and what a release needs from it.
`.github/workflows/` is the reference; `just guards` lints it and
`just tools-check` tests its contracts.

## Routing

The `plan` job classifies the diff (`bun tools/main.ts detect-changes` over
[`routes.ts`](../tools/src/routing/routes.ts)) and picks the lanes it can
affect. Unknown paths fail closed and run everything. A library change runs
its dependents' tests. Handwritten docs normally run only guards; docs that
tests read select those tests.

Each lane is one job running the recipes that
[`plan.ts`](../tools/src/routing/plan.ts) names for it, so `just check`
locally runs the same commands as CI. The lane-to-recipe table and each
event's policy live in that file; do not copy them elsewhere.

| Event | Lanes |
| ----- | ----- |
| draft pull request | none; reports `ci (draft)` ([arming](#drafts-and-automated-arming)) |
| pull request | what the diff affects, Linux only; native setup and rust icount path-scoped |
| release bot PR (`release-please--*`, `rune-release/*`) | full qualification, Apple included |
| merge group, `main` push | full Rust pool plus what the diff needs; native setup and rust icount mandatory |

The `ci` job aggregates the lanes and is the required check; a skipped lane
passes only where the plan allowed the skip.

## Drafts and automated arming

A draft, release trains included, skips the plan and reports only
`ci (draft)`, with no receipt, so required `ci` is absent rather than red.
Renaming cannot revoke an older same-SHA `ci`, so automation keeps
auto-merge off until this read-only gate passes:

```sh
bun tools/main.ts pr-ready-ci PR_NUMBER EXPECTED_FULL_SHA
```

It needs an open, ready, same-repository PR on `main` at that head whose
newest normal `ci` run (successful `plan`, `ci` and `Strict CI aggregate v2`)
and `commitlint` started after the latest ready transition. Ambiguous,
pending, stale or missing evidence refuses; never rerun CI or cycle
draft/ready to get past it. The caller binds the merge to the head
(`gh pr merge --match-head-commit`).

## Receipts

A `main` push whose exact tree a pull request or merge group already
validated with the same lanes reuses that receipt
([`receipt.ts`](../tools/src/routing/receipt.ts)) and runs only the lanes
`reused` in `plan.ts` keeps (ffi, native setup, rust icount).

## After the merge

`main` has no merge queue. A pull request merges once `ci` and `commitlint`
pass on its own diff; the slow lanes first run on the `main` push. A red
`main` blocks a release, not a merge. When a `ci` or `rune-macos` push run on
`main` fails, [`main-red.yml`](../.github/workflows/main-red.yml) opens one
`ci/main-red` issue listing the suspect commits and closes it once every
tracked workflow is green again. Fix forward or revert the suspect.

## Native Apple qualification

`ci.yml`'s `rune` lane is Linux. `rune-macos.yml` runs macOS and the iOS
simulator on `main` pushes, nightly, and by dispatch. On a merge group and a
Rune-affecting release PR, the `ci` aggregate calls `rune-macos.yml` and
requires it; a skipped, failed or cancelled child fails `ci`. Other pull
requests get a cheap standalone `rune-macos` check that covers no Apple work.
The required child has read-only permissions and no signing secrets.

What a release needs:

- **Stable CLI draft** (`release.yml`): a green `main` push `ci` for the
  exact tagged SHA, plus a full native receipt (both Apple legs, same tree).
- **Rune tag** (`bun tools/main.ts ci-green SHA`): the same push proof and
  native receipt. Only when no push run exists for that SHA, one successful
  `merge_group` run of it counts instead, if `main` contains the SHA, every
  job the push plan requires passed (or an identical-tree receipt proves it),
  and its called `rune-macos` passed
  ([`queue-ci.ts`](../tools/src/release/queue-ci.ts)). A push run still in
  progress blocks the tag.

Missing or expired proof holds a draft; rerun the qualified push.

## Concurrency

- `main` pushes: one group per SHA, never cancelled. Routing compares each
  push's own range, so replacing a pending push would drop its validation.
- Pull requests: updates and the closing event share one latest-wins group
  keyed `<head repo>:refs/heads/<head ref>`, so a merged PR's close cancels
  its run in flight.
- Dispatches: `<repo>:<ref>`, so a tag cannot cancel a same-named branch.
- Merge groups: their own queue ref.

Colons separate the parts because neither repository nor branch names allow
them. `rune-macos.yml` prefixes its groups `required:` or `standalone:`, unlike
the calling `ci`, so the required child cannot cancel itself or be cancelled
by the cheap check.

## Runners and caches

Runners are hosted Ubuntu x64 and arm64 and macOS. The test lanes compile once
(`just test-archive`) and run unit and e2e shards from that nextest archive.
Pull requests restore caches and never save them. `main` pushes and
[`cache-warm.yml`](../.github/workflows/cache-warm.yml) save them: 00:10 UTC
for the Linux and browser lanes, 06:17 UTC for Rune.
