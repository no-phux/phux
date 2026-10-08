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

## Key echo (coordinator path)

`scripts/measure-key-echo.sh` (also `./scripts/measure.sh measure-key-echo`)
is the regression guard for the path a `cmd+T` terminal now takes: a text key
leaving the Phux host, through the FFI, the socket, the coordinator's PTY and
the shell's echo, until the published grid's damage lands back in the host.
The sample is taken by the host's own clock (`EchoProbe` in
`src/providers/phux/host.zig`, on only while `PHUX_COCKPIT_KEY_ECHO` is set),
one key in flight at a time, and each sample is one `key_echo_us=` line in
the app log. What is left between that and glass is the SDK's paint and
present, which the same run prints from its frame profile when the ring is
full; the two are reported side by side, never summed into a synthetic
keystroke-to-glyph percentile.

The run packages the checkout with automation, identity-stages it with its
own HOME, config, state and socket, and lets the bundled CLI start an
isolated coordinator; a developer's `PHUX_SOCKET` is overridden on purpose.
It then activates the app by PID (typed keys are refused unfocused), which
needs a logged-in GUI session and, once, macOS Automation permission for the
invoking terminal to control System Events, and types 160 single letters
30ms apart. Below 128 samples it refuses to report. `--max-p99-us` is the
drift gate: a p99 above it fails the run. The default ceiling is provisional
until the first permitted run pins it; replace it with that run's p99 times
two and cite the `MEASURED-BASIS` line here.

## Paint ceilings (Hybrid C)

`scripts/measure-paint-ceiling.sh` (also `./scripts/measure.sh measure-paint-ceiling`)
runs the headless native suite with `-Dmeasure=true` and prints the SDK paint
tables, unbounded 320x96 bind points, and Hybrid C vs equal-cut fleets at
N=1/2/4/8. It does not bump the Native SDK pin. Live PTY rss is
`scripts/drive-shell-ceiling.sh` and is macOS-only. Cockpit `zig build`
itself is macOS-only (`build.zig` panics on other hosts); Linux agents
derive the same tables from the pinned SDK sources under `zig-pkg/native_sdk-*`
and leave the runnable measurement to macOS CI.

The policy those numbers feed is [DECISIONS.md](DECISIONS.md) §"Paint ceilings:
Hybrid C". Degraded panes crop last-N at `max_rows/4`. After the signed
cell bump leftover after one full grid exceeds that cap, so the 24-row
cap binds. The regression that `layout.max_panes` full product grids do
not share one envelope lives in `src/cockpit/native/paint_budget.zig`.

