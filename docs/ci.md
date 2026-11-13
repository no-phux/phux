# CI

The hosted counterpart of [local validation](./development.md#validating-a-change).
`.github/workflows/` is the executable reference; `just guards` lints it and
`just tools-check` tests its contracts.

## Routing and receipts

One classifier, `bun tools/main.ts detect-changes` over
[`tools/src/routing/routes.ts`](../tools/src/routing/routes.ts), decides
which lanes a change can affect. `ci.yml` owns all these lanes. Unknown
paths fail closed and run everything; handwritten docs normally run only
guards. Docs read by tests, including `docs/agents.md` and generated
references, also select their Rust consumers. A library change runs the
tests of its dependents. The required `ci` aggregate rejects a failed or
cancelled lane. `just tools-check` tests the routes.

Release metadata alone runs guards without compile lanes. The classifier
compares both full documents: root workspace and lock versions, release
manifest versions, skill version lines, changelogs, and top-level `version`
in the five integration packages listed in `release-please-config.json`.
Any other package field, missing side, unlisted path, or workspace bump
without all its locks keeps normal routing.

`main` pushes and merge queue groups (`merge_group`) run main policy: the
full Rust pool and every lane the diff needs. A `main` push whose tree a
pull request or merge group already validated with the same lanes reuses
that receipt (`tools/src/routing/receipt.ts`). With the repository variable
`PHUX_MERGE_QUEUE=true`, a pull request that reaches Rune only through a
root crate in its Cargo closure skips `rune / ios-native-simulator`. Its
merge group still runs it, and its receipt never covers a `main` push.

## Concurrency

Ordinary main pushes share one CI concurrency group: a running push finishes,
and a newer push replaces only the pending one. Release App pushes and
conventional `chore: release ...` / `chore(rune): release ...` commits retain
per-SHA groups, including manual merges. A manually tagged ordinary commit
whose pending push was superseded still needs its own push CI rerun before
it can release; a dispatch or another commit's green cannot substitute.

PR updates, closes and a dispatch of the same repository branch share a
latest-wins group. A merged PR's closed event carries the base ref, so its
key uses the head repository and `refs/heads/` plus `head_ref`, not that
base ref. Dispatches use their repository and full ref: a same-name tag
cannot cancel a branch. The repository/ref separator is a colon, forbidden
in both names, so different repository/branch splits cannot collide.
Merge groups retain their own queue-ref group.

First-run timing runs Linux on every event. Its Mac job runs for installer
changes on PRs and main pushes, nightly, and on manual runs. Only selected
Mac jobs coalesce: unrelated pushes cannot replace a pending installer
qualification, and nightly/manual jobs have separate groups.

## Runner and cache budget

The test lanes compile once (`just test-archive`) and run the unit and e2e
shards from that nextest archive. Pull requests restore caches and never
save them; `main` pushes save one entry per lane under a 10 GB repository
budget, pruned by `prune-build-cache`, warmed by `cache-warm.yml`.
Runners are the free hosted ones:
Ubuntu x64 and arm64, and macOS 26 plus the `xcode-27` image.
