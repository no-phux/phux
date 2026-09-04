# Measurement harness

Start with `./scripts/measure.sh`. It discovers runnable measurements from the
`# measures:` line in each script rather than maintaining a second catalog that
can drift.

## Rules

1. A pinned number names the command that derives it. A number without a
   re-runnable derivation is a guess wearing a constant's clothes.
2. Measurements print `MEASURED-BASIS` before results. Existing `MEASURED`
   records remain stable because they are cited verbatim elsewhere.
3. The pinned SDK keeps a rolling 128-sample ring per stage. Snapshot `_n` is a
   lifetime total, not percentile population; the harness therefore prints
   `*_population_n=min(*_n,128)` and never permits a floor above 128. Each
   stage's p50, p90, and max are printed only if that actual population
   independently meets `MEASURE_SAMPLE_FLOOR`. The four-pane scheduler run
   refuses success until every pipeline stage plus the `interval` delivery
   channel holds a full rolling population. Churn independently requires full
   populations for its topology stages (`rebuild`, `layout`, `reconcile`,
   `emit`, `a11y`, `plan`, `patch`, and `encode`).
4. Driven app measurements launch through `scripts/lib/dev-app.sh`: the bundle
   is identity-staged and re-signed, config and state are isolated, and both app
   and CLI run from a private working directory so they share a private
   automation dropbox. `scripts/lib/app-instance.sh` still binds every read to
   the launched pid and refuses a same-name sibling or an empty tree. A
   retained `--keep` run prints the exact cwd-pinned wrapper command and
   dropbox path needed to inspect that same instance.
5. Dependency pins continue to be read only by `scripts/lib/zon.sh`. A local
   `.path` override is a named refusal, never permission to consume the next
   dependency's URL.

## Live Automation Evidence

Both commands below package the current checkout with automation, identity-stage
the real `.app` through the same `dev_app_stage` machinery as `dev-run.sh`, and
run app and CLI from one private home. `app-instance.sh` binds every observation
to the exact publisher PID and refuses a same-name sibling, publisher swap, or
empty tree.

```sh
./scripts/automate-smoke.sh --fullscreen
./scripts/automate-smoke.sh --profile
```

`--fullscreen` requires a logged-in macOS GUI session and Accessibility
permission for the invoking terminal. It activates the staged bundle by Unix
PID, reads the frontmost PID back, proves the automation snapshot says the
window is focused, and then observes the same window's AppKit `AXFullScreen`
attribute move `false -> true -> false` around two driven `ctrl+cmd+F` chords.
The initial and return states are the negative controls. No screenshot is used:
the automation PNG is a CPU reference rendering of the retained canvas and
cannot prove an OS window transition.

`--profile` proves each split by an absent-then-present pane address and checks
the exact pane count after every action. It starts a fresh profile only after
four continuously repainting panes exist, then waits for full 128-sample rings
for `rebuild`, `layout`, `reconcile`, `emit`, `a11y`, `plan`, `patch`, `encode`,
`present`, `host_decode`, `host_draw`, and `interval`. The output reports each
stage's p50/p90/max and puts the interval p50/p90/max beside the sum of the
stage p90s. That sum deliberately double-counts nested present/host work; it is
an attribution comparison, not a synthetic frame percentile. An interval p90
above even that conservative sum distinguishes delivery gaps from measured
synchronous stage cost.

## Adding A Measurement

Add `# measures: <description>` to a runnable script, source
`scripts/lib/measure.sh`, print `measure_basis` before scalar results, and use
`measure_print_frame_profile` or `measure_require_sample_floor` for statistics.
Every driven assertion needs a negative control or an explicitly checked state
transition.

Stored machine-dependent baselines and a wrapper that normalizes every
instrument's flags are deliberately omitted. Re-run derivations and compare
the same basis instead.
