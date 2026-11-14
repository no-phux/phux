# CI execution plan

The [`plan` job](../.github/workflows/ci.yml) routes the diff through
[`tools/src/routing`](../tools/src/routing/plan.ts) to lanes. Each lane is
one job, except web, which is two parallel jobs: `web-vt` and `web-client`.
A lane sets up its environment and runs the just recipes `plan.ts` names
for it, so `just check` locally runs the same commands.

A lane the diff cannot affect is skipped. On a pull request the `ci`
aggregate (the required check) accepts that skip. Only an affirmative live
base-branch rules API response containing `merge_queue` permits PR fast paths.
Missing enforcement, malformed context, or API/permission errors qualify the
PR instead. `native setup` and `rust icount` are then mandatory, as on
`merge_group` and `main`: a skip fails the aggregate.
A merge group plans like a main push.
On the queue, and Rune-affecting fallback PRs (including release metadata),
the same required aggregate also calls and requires the
`rune-macos` child. A skipped, failed or cancelled Apple child is not a pass.

A main push whose tree a pull request or merge group already validated
with the same lanes skips the lanes the receipt covers and still runs
native setup and rust icount. The binding lane also remains selected when
the main plan needs it. See
[`receipt.ts`](../tools/src/routing/receipt.ts) for exact-tree receipt
verification and [`reused`](../tools/src/routing/plan.ts) for the lanes
kept on reuse. The aggregate waits for both web jobs; a failure or
cancellation in either fails validation.

Rune's queue-verified PR Linux lane runs `check.sh` and cargo-deny on x86_64.
Fallback PRs, queue/main and manual dispatches additionally package and launch both Linux hosts.
Only those full Linux receipts include `linux-aarch64,packaging`; a partial
PR receipt cannot replace that qualification. Tags and nightly Rune warms
retain full qualification by default.

The Rune lane here is Linux-only. Its receipt never claims Apple coverage.
The Apple workflow owns its exact-tree macOS/iOS receipt and release proof;
see [native qualification](./ci.md#native-apple-qualification).
