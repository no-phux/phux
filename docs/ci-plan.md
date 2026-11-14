# CI execution plan

The [`plan` job](../.github/workflows/ci.yml) routes the diff through
[`tools/src/routing`](../tools/src/routing/plan.ts) to lanes. Each lane is
one job, except web, which is two parallel jobs: `web-vt` and `web-client`.
A lane sets up its environment and runs the just recipes `plan.ts` names
for it, so `just check` locally runs the same commands.

A lane the diff cannot affect is skipped. On a pull request the `ci`
aggregate (the required check) accepts that skip. `native setup` and
`rust icount` are required on `merge_group` and `main`: a skip there fails
the aggregate. A merge group plans like a main push.

A main push whose tree a pull request or merge group already validated
with the same lanes skips the lanes the receipt covers and still runs
native setup and rust icount. The binding lane also remains selected when
the main plan needs it. See
[`receipt.ts`](../tools/src/routing/receipt.ts) for exact-tree receipt
verification and [`reused`](../tools/src/routing/plan.ts) for the lanes
kept on reuse. The aggregate waits for both web jobs; a failure or
cancellation in either fails validation.
