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

Release metadata alone runs guards without compile lanes. The classifier
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

`ci.yml`'s `rune` lane is Linux. A pull request runs `check.sh` and cargo-deny
on `linux-x86_64` only: no package, no packaged launch, and no
`linux-aarch64`. Its receipt omits `linux-aarch64` and `packaging`, so a
main push cannot reuse it for those. The merge queue, a main push, and a
manual `ci` dispatch package and launch both Linux hosts. Nightly
`cache-warm.yml` does the same with the default inputs. `rune-client.yml`
does too, and its preflight still requires the green main `ci` that carried
that qualification. A pull request reports a passing `rune-macos` check
without starting Apple runners; that receipt covers no Apple work.
`rune-macos.yml` runs macOS and the native iOS simulator/device
qualification on main pushes, nightly, and manually. Every Rune-affecting
queue group runs both Apple legs, whether reached directly or through Cargo.

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
A red Apple result on a direct main push does not block main; it blocks
publication when Rune is in that release's closure. See [releasing](./releasing.md).

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

## Runner and cache budget

The test lanes compile once (`just test-archive`) and run the unit and e2e
shards from that nextest archive. Pull requests restore caches and never
save them; main pushes save one entry per lane under a 10 GB repository
budget, pruned by `prune-build-cache`, warmed by `cache-warm.yml`.
Runners are free hosted Ubuntu x64 and arm64, macOS 26 and `xcode-27`.
