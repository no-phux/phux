---
audience: contributors
stability: stable
last-reviewed: 2026-08-09
---

# 0079 — Fatal-signal terminal restore

**TL;DR.** A SIGSEGV/SIGBUS/SIGABRT in the client does not unwind, so neither
`RawModeGuard::drop` nor the panic hook runs and the user is left in raw mode
inside the alt screen with mouse tracking armed. phux now installs an
async-signal-safe handler that writes the DEC private-mode resets with a raw
`write(2)`, restores the saved termios, and re-raises with default disposition
so core dumps and exit status are unchanged. The handler lives in
`phux-crash`, a crate vendored from xAI's Apache-2.0 `xai-crash-handler` — the
only crate in this workspace that is not `MIT OR Apache-2.0`.

Status: Accepted
Date: 2026-08-09

## Context

phux already restores the terminal on every teardown path that gives it a
chance to run:

- **Normal exit and detach** — `RawModeGuard::drop` restores termios from the
  instance field and calls `write_terminal_reset`.
- **Panic** — `install_panic_hook_once` logs to the file sink, calls
  `terminal_reset_on_signal`, then chains the previous hook.
- **SIGINT / SIGTERM / SIGHUP** — handled in the main `select!` loop, each arm
  calling `terminal_reset_on_signal` before exiting with the shell-conventional
  code.

Every one of those depends on ordinary control flow reaching a cleanup path.
A fatal signal reaches none of them. `SIGSEGV` does not unwind, so `Drop` never
runs; it is not a Rust panic, so the panic hook is never called; and it is not
in the `select!` set, so no arm fires. The process dies with default
disposition and the outer terminal keeps whatever phux last set: `?1049h`
(alt screen), `?25l` (cursor hidden), `?1002h`/`?1006h` (mouse reporting), and
raw termios. The user gets an unusable shell and has to know to type `reset`
blind.

This is not hypothetical for phux specifically. `phux-client` is
`#![forbid(unsafe_code)]`, so its own code is not where a segfault comes from.
The crash surface is the native `libghostty-vt` FFI boundary — a Zig terminal
engine ([ADR-0004](./0004-libghostty-vt-as-grid.md)) that the client runs
per-attached-pane in-process ([ADR-0013](./0013-libghostty-bytes-on-wire.md)).
We have already shipped fixes for two engine-level crashes (`phux-y06`, a
`PageList.resizeCols` overflow on a simultaneous shrink-both-dims resize;
`phux-h5hj.8`). Each of those, before its fix, wedged the terminal of every
user who hit it. The engine is upstream code on a pinned git rev; the next
such bug is a question of when, not whether.

The existing comment at the panic-hook call site already names the risk —
"renderer bug, libghostty FFI surprise, etc." — but a panic hook cannot see
the FFI surprise in the form it actually takes.

## Decision

1. **Install an async-signal-safe fatal-signal handler for SIGSEGV, SIGBUS,
   and SIGABRT.** It writes a fixed byte string of DEC private-mode resets
   directly to fd 2 with `libc::write`, restores termios with `tcsetattr` from
   a snapshot taken before raw mode was entered, then restores `SIG_DFL` and
   re-raises. No allocation, no locks, no libc buffering — nothing that is
   unsafe to call from signal context. Re-raising rather than exiting keeps
   core-dump behaviour and exit status exactly as they were.

2. **Vendor `xai-crash-handler` as `crates/phux-crash` rather than write our
   own.** It is a complete, tested implementation of a fiddly piece of systems
   code: an alternate signal stack via `mmap` so the handler survives a stack
   overflow, `SA_RESETHAND` so a fault inside the handler cannot loop,
   ordering constraints on the reset sequence that are enforced by unit tests,
   and an integration suite that spawns real subprocesses which really do
   raise each signal and asserts on what they left behind. Upstream is
   Apache-2.0 and has no internal dependencies — only `backtrace` and platform
   `libc`/`windows-sys`.

3. **The reset sequence is deliberately a superset of what phux enables.**
   phux sets `?1049`, `?25`, `?1002`, `?1006`, `?1003`, and `?2026`. The
   sequence also clears `?1000`, `?1015`, `?2004`, `?1004`, and pops the kitty
   keyboard stack — modes phux parses on input but never requests. Resetting a
   mode that was never set is a no-op at the terminal; missing one costs the
   user a wedged session. Two ordering constraints are load-bearing and
   test-enforced: synchronized update (`?2026l`) ends **first**, so a
   multiplexer stops buffering before the rest arrives, and the kitty pop
   precedes `?1049l`, because the protocol stack is per-screen.

4. **Escape-code restoration is armed and disarmed with the alt screen.**
   `phux_crash::install_terminal_restore_only()` is called in `attach_session`
   *before* `RawModeGuard` — the handler snapshots termios at install time, so
   installing it later would capture the raw flags and "restore" the user into
   raw mode, the exact wedge this ADR exists to prevent. That call arms the
   termios-only variant. `RawModeGuard::install_with_stdout` upgrades to the
   escape-code variant once the alt screen is genuinely up, and `Drop`
   downgrades it again, so a crash on the normal screen never sprays DECSETs
   across it.

5. **`phux-crash` is Apache-2.0 only, and says so.** Its `Cargo.toml` sets
   `license = "Apache-2.0"` explicitly rather than inheriting the workspace's
   `MIT OR Apache-2.0`, it ships its own `LICENSE` and `NOTICE`, and every
   modified file carries a `MODIFIED FROM UPSTREAM` banner. The full
   modification list lives in the crate's `Cargo.toml` header and is the
   re-vendoring checklist.

6. **Crash *reporting* is deliberately out of scope here.** The upstream crate
   can also write a binary crash blob and symbolicate it on the next start
   (`install()` + `check_previous_crash()`). We install only the terminal-
   restore half. Turning on blob capture means deciding where blobs live, how
   long they are kept, whether the next launch surfaces them, and whether any
   of it is reported anywhere — a product question, not a terminal-hygiene
   one. The capability is present in the crate when we want it.

## Rationale

The alternative framings all lose to the same fact: the failure mode is not
"we crashed", it is "we crashed *and took the user's terminal with us*". The
first half is a bug to fix upstream; the second half is a property phux can
guarantee unilaterally, cheaply, for any cause of death.

Vendoring beats writing it ourselves because the difficulty here is not the
idea — everyone knows you want to reset the terminal on a segfault — it is the
signal-safety discipline. Anything that allocates, takes a lock, or goes
through buffered I/O is a potential deadlock in a handler that ran because
memory is already corrupt. Upstream's version has the alternate stack, the
recursion guard, the reset ordering, and a test suite that raises the actual
signals. Reimplementing that to avoid a license note would be vanity.

The `unsafe` is real, and confining it to its own crate is the point: it is
raw pointer work and `sigaction` FFI that cannot be expressed in safe Rust,
and putting it behind a crate boundary lets `phux-client` keep
`#![forbid(unsafe_code)]` unchanged. This is the same shape as
`portable-pty-adopt` ([ADR-0032](./0032-graceful-in-place-upgrade.md)):
narrow, dependency-clean, `unsafe` where it must be, isolated from everything
else.

## Tradeoffs

- **A non-dual-licensed crate in a dual-licensed workspace.** Anyone who needs
  the MIT option cannot link `phux-crash`. Since it is a leaf dependency of
  `phux-client`, that effectively means the binary as shipped is Apache-2.0.
  This is a real narrowing of what [ADR-0071](./0071-what-phux-1-0-commits-to.md)
  freezes, and the reason it is stated in three places rather than one.
- **Process-global signal handlers.** Registering SIGSEGV/SIGBUS/SIGABRT is a
  process-wide act. Upstream's integration tests cover coexistence with a
  running tokio runtime and prove other signals are not clobbered, but any
  future in-process host that wants its own fatal-signal handling will have to
  reconcile with ours.
- **Nothing is captured about *why* we crashed.** Per decision 6, the terminal
  is saved and the crash is not explained. A user who hits an engine fault
  still has nothing to send us beyond "it died". Blob capture is the follow-up.
- **We now carry vendored code.** It must be re-synced by hand if upstream
  fixes something. The modification list is kept in the crate's Cargo.toml
  specifically so that re-sync is a diff-and-reapply rather than an
  archaeology exercise.

## Alternatives considered

- **Write a minimal handler ourselves.** Perhaps 40 lines for the happy path.
  Rejected: the happy path is not the hard part. Without the alternate signal
  stack, a crash caused by stack overflow — a real possibility in a recursive
  layout tree — cannot run the handler at all, and without `SA_RESETHAND` a
  fault inside the handler spins.
- **Use an existing crash crate (`human-panic`, `color-eyre`, `sentry`).**
  Rejected: the first two run inside the panic hook, pre-unwind, which is
  precisely the case phux already handles. They never execute for a segfault.
  `sentry`'s native handler would work but brings a reporting SDK and a
  network surface for what is a five-escape-sequence problem.
- **Reimplement upstream's logic from scratch to keep MIT.** Rejected as
  license-driven engineering: it would produce less-tested code with the same
  behaviour, and the Apache grant is confined to one leaf crate either way.
- **Do nothing; treat it as an upstream libghostty problem.** Rejected: it is
  true that the fix for any *specific* crash belongs upstream, and equally
  true that the user's terminal should survive a crash we have not fixed yet.
  These are not competing.
