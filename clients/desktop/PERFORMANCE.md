# Performance monitor

Open the command palette and search **Performance Monitor**, **CPU**, **memory**,
or **telemetry** to opt in. This is a utility, not a separate Developer menu.
It opens a native window and starts sampling immediately; neither the desktop
nor the server needs a restart. Reopening the command raises the same window.

While monitoring, the palette also offers **Stop Performance Monitoring**.
This closes the monitor and stops its sampler. Closing the window does the
same. You can enable it again immediately; each new monitor starts a fresh CPU
and metric interval.

For automatic opening at launch, the native executable accepts `--dev-telemetry`
or `PHUX_DESKTOP_DEV_TELEMETRY=1`. These are conveniences, not prerequisites for
the runtime controls.

The monitor samples the **desktop process** once per second, and shows the
server's CPU, memory and heap every two seconds by running `phux perf
--json`. Terminal child processes are not included. **Show HUD** and
**5 s Trace** are also available through the [dev-mode tools](USAGE.md#dev-mode).
CPU is measured over the actual elapsed interval
(one fully occupied core is 100%, so multiple cores can exceed 100%). Current
resident memory and lifetime peak resident memory are separate readings.
An em dash means unavailable or no interval yet, not zero.

**Overview**, **Memory**, and **Metrics** separate quick readings from the raw
instrumentation. Overview has hoverable Longbridge Kit CPU and app-memory charts.
App memory means macOS physical footprint (closest to Activity Monitor), or RSS
on Linux. Memory also shows native malloc blocks in use and reserved capacity
on macOS, without requiring Rust counting. Capacity is not resident RAM.
Charts retain at most 120 numeric samples, not
full reports, and restart after a pause. The content scrolls even in a short
window. Space pauses or resumes unless you are typing in a field; Escape, ⌘W
or Ctrl-W closes the monitor. ⌘F or Ctrl-F opens the metric filter; every search
word must match the metric name.

**Set baseline** compares later memory readings with the current snapshot and
restarts chart history without resetting counters. Repeat the same workload
and compare after it settles. **Release unused heap** asks macOS malloc, off
the UI thread, to return unused pages. It leaves live allocations intact and
reports the allocator's returned-byte count, which is not necessarily the
footprint reduction. It is best-effort, macOS-only, and disabled while paused.

Footprint, RSS and tracked Rust heap overlap: do not add them or subtract heap
from RSS to infer GPU memory. Native AppKit, Metal, window surfaces and
allocator-retained pages are outside the Rust allocation counter. The Memory
tab explains the measurements and gives the process-specific `vmmap -summary`
command for native attribution. Rust counting requires an isolated development
launch with `PHUX_DEV=1`, **Settings > Performance > Count Rust allocations**,
or **Count allocations** directly in the monitor's Memory tab.
The setting applies immediately across desktop windows without a restart and
lasts only for this run. It does not enable a dev socket or change the server.
Turn it off after profiling to remove counting overhead. Opening the monitor
never enables counting automatically.

The scrollable metric table shows the existing kernel, runtime, renderer, and
UI-thread instrumentation as interval rates, mean timings, and estimated p95.
**Pause** freezes the readings and releases this monitor's server polling watch;
another open HUD may still poll. **Resume** starts a fresh
interval. **Copy JSON** copies the displayed process readings, interval metrics,
and cumulative report. Sampling stops when the monitor closes; use the palette
to reopen it. This is local instrumentation: no telemetry is uploaded and no
metrics history is written by the monitor.

Without opening the monitor, there is no monitor sampling loop. The existing
cheap atomic render/runtime counters remain on. Nothing about enabling or
stopping this monitor changes the server.
