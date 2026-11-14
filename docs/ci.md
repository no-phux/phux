# CI

The hosted counterpart of [local validation](./development.md#validating-a-change).
`.github/workflows/` is the executable reference; `just guards` lints it and
`just tools-check` tests its contracts.

## Routing and receipts

One classifier, `bun tools/main.ts detect-changes` over
[`tools/src/routing/routes.ts`](../tools/src/routing/routes.ts), decides
which lanes a change can affect. Unknown paths fail closed and run everything;
handwritten docs normally run only guards. Docs read by tests, including
`docs/agents.md` and generated references, select their Rust consumers.
A library change runs the tests of its dependents.

Release metadata on a PR runs guards without compile lanes. Main and queue
always run native setup and rust icount, including after receipt reuse.
Their PR selection is scoped to the job's own paths (or `perf-baseline` for
icount); unknown or unreadable scope fails closed. The classifier
compares both full documents: root workspace and lock versions, release
manifest versions, skill version lines, changelogs, and top-level `version`
in the five integration packages listed in `release-please-config.json`.
Any other package field, missing side, unlisted path, or workspace bump
without all its locks keeps normal routing.

`main` pushes and merge queue groups (`merge_group`) run main policy: the
full Rust pool and every lane the diff needs. A `main` push whose tree a
pull request or merge group already validated with the same lanes reuses
that receipt (`tools/src/routing/receipt.ts`).

## Native Apple qualification

`ci.yml` runs the Linux Rune hosts. A pull request reports a passing
`rune-macos` check without starting Apple runners; that receipt covers no
Apple work. `rune-macos.yml` runs macOS and the native iOS simulator/device
qualification on main pushes, nightly, and manually. Every Rune-affecting
queue group runs both Apple legs, whether reached directly or through Cargo.
Recognized release metadata also selects both Apple legs, even when CI's
product lanes are unselected. This qualifies the metadata release's own tree,
including native code behind any intervening documentation-only commits.

On `merge_group`, **the already-required `ci` aggregate calls and requires
the Apple workflow**. It rejects a skipped, failed, or cancelled child.
No additional required check, queue drain, or ruleset cutover is needed.
Old queue entries retain their original full native `ci`. The child is
not also triggered independently on the queue, so qualification is not doubled.

A main push can reuse the exact native receipt from its successful queue
`ci` caller: same immutable tree, source repository, referenced native child
revision, workflow and both-leg
coverage. PR and Linux-only receipts cannot attest Apple work.
Nightly and manual runs always qualify the full tree.
Native main pushes upload full-coverage receipts after the aggregate succeeds.
Every stable draft and Rune tag requires that exact release SHA's successful
main push plus full native receipt: same tree, workflow and both-leg coverage.
A skipped/cheap green, ancestor receipt, other SHA, PR or dispatch cannot
substitute. Missing or expired proof blocks publication.

This deliberately qualifies even integration-only metadata releases rather
than introducing ancestry tracking or product-specific exemptions. Arbitrary
tags at unqualified docs-only commits are refused; use a normal version bump.
A red Apple result on a direct main push does not block main, but cannot ship.
See [releasing](./releasing.md).

Missing or expired receipts hold a draft; rerunning the original qualified
push can refresh its proof. A batch merge may produce a push only for its
final commit. Intermediate version commits without their own push run remain
blocked under the exact-push policy. Another batch SHA, arbitrary queue/PR
green or a dispatch is not a substitute; qualified intermediate-main
eligibility is a separate policy decision.

## Concurrency

Every main push retains its per-SHA CI **and Apple** groups, with no
cancellation of running pushes. Routing compares each push's before/after
range: replacing a pending native change with a docs-only push would lose
its qualification. Actor identity never changes admission. This matches
the reviewed CI repair (#2462), not rejected coalescing proposal #2449.

PR updates, closes and a dispatch of the same repository branch share a
latest-wins group. A merged PR's closed event carries the base ref, so its
key uses the head repository and `refs/heads/` plus `head_ref`, not that
base ref. Dispatches use their repository and full ref: a same-name tag
cannot cancel a branch. The separator is a colon, forbidden in both names,
so different repository/branch splits cannot collide.
Merge groups retain their own queue-ref group. The Apple child's group
prefix differs from its calling `ci`, preventing self-cancellation.

First-run timing runs Linux on every event. Its Mac job runs for installer
changes on PRs and main pushes, nightly, and on manual runs. Only selected
Mac jobs coalesce: unrelated pushes cannot replace a pending installer
qualification, and nightly/manual jobs have separate groups. Its output-file
fixture uses a regular temporary file, like Actions, not a captured `/dev/stdout`.

## Runner and cache budget

The test lanes compile once (`just test-archive`) and run the unit and e2e
shards from that nextest archive. Pull requests restore caches and never
save them. Main pushes and the 00:10 UTC Linux-only cache-warm dispatch
save per-lane caches under the 10 GB budget. Post-upload pruning keeps
current-day keys and never deletes the sole older restore source before
a stored replacement exists. Rune warming keeps its separate 06:17 clock.
The same nightly Linux dispatch warms both browser dependency caches using
the CI web recipes. It explicitly enables saves on this repository's `main`;
PRs and merge groups stay restore-only. This seeds the caches even when an
exact-tree queue receipt skips the web jobs on the subsequent `main` push.
PR and topic-ref entries are outside the pruner's deletion scope.
Runners are free hosted Ubuntu x64 and arm64, macOS 26 and `xcode-27`.
