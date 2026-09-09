---
audience: contributors, agents
stability: stable
last-reviewed: 2026-09-08
---

# Retired regression guard workflow

**TL;DR.** Cockpit's permanent regression-patch ledger is retired. Ordinary
behavioral tests gate PRs; bug fixes retain one-time evidence that the named
test failed against the actual defect and passed after repair. Separate scoped
mutation reports help find assertion gaps. Historical guard records describe
past runs, not a current requirement to maintain patches.

The current workflow lives in the repository's
[mutation testing policy](../../../docs/TESTING_MUTATIONS.md). Tests and their
defect explanations remain in the source tree. The 115 `.guard` files, source
markers, `guard-check.sh`, `guard-red-run.sh`, and `addGuardCheck` build hookup
were removed. Their prior versions and recorded RED evidence remain in Git
history; acceptance records retain their original scope and results.

## Why the ledger existed

On 2026-08-10 a regression test passed against the very bug it was written to
catch. It drove a window resize with four panes open and allowed a few more
frames for layout to settle. The old behavior converged one pane per frame,
so four panes needed exactly four frames, inside the allowance.

Disabling the fix exposed the ineffective assertion. The test was tightened
to require exactly one viewport dispatch. This was the origin of the historical
`one-frame-convergence` guard and the rule to observe an actual regression
failure rather than infer coverage from a green test.

## Why it was retired

The old build gate checked marker/file correspondence, recorded RED metadata,
and whether each hand-authored break still applied. Patch applicability never
proved that the test still detected the defect. Moving sound code could break
the patch ledger without breaking behavior, requiring repeated bookkeeping
and full test runs to re-record historical evidence.

The useful evidence is the original bug reproduction and the retained behavioral
test. Record that RED/GREEN result once when fixing a bug. Use automatic,
opt-in, diff-scoped mutation scans separately to probe current assertions;
review survivors as findings rather than impose a 100% kill quota or commit
permanent mutation patches. A compile failure or unrelated test failure is
not evidence that the intended assertion caught the defect.
