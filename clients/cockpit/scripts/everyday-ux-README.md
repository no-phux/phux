---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-12
---

# Everyday UX smoke

**TL;DR.** Run this serial-only driver against the checkout you want to inspect.
It builds and launches an isolated real Cockpit bundle, sends AppKit input, and
retains product assertion failures separately from instrumentation failures.

## Run

Follow [setup](../../../docs/SETUP.md#cockpit) first. Quit other Cockpit test
instances and coordinate exclusive live-app ownership before running:

```sh
NATIVE="$(clients/cockpit/scripts/build-automation-cli.sh)"
python3 clients/cockpit/scripts/everyday-ux-smoke.py --native "$NATIVE"
```

The default run rebuilds same-checkout FFI and the release-mode automation
bundle, with two Cargo/Zig build jobs. On a busy parallel-build host, set
`PHUX_ZIG_BUILD_TIMEOUT=1800` to allow a cold package build longer than the
wrapper's default ten-minute deadline. `--phux /absolute/path/to/phux` selects the fixture server executable;
the default uses the existing CLI on PATH, without upgrading it. `--artifacts`
accepts a new directory; the default is under `/private/tmp/opencode`.
`--no-build` is useful for instrument development, but explicitly records an
**unverified source/build binding**. Use the default build for acceptance.
The build explicitly binds Cargo output and Zig's FFI include/archive inputs
to this checkout, overriding inherited foreign archive locations. It reads the
native host triple from `rustc -vV`, clears `CARGO_BUILD_TARGET`, and passes
`cargo rustc --target <host>` explicitly. This outranks Cargo configuration's
`build.target`. Zig consumes exactly
`target/<host>/ffi-dev/libphux_client_ffi.a`; the triple-qualified directory is
required even when the selected target equals the native host. Provenance records
that target and hashes that exact archive, never `target/ffi-dev` left by an older build.

The script stops only the app and server it launched. Artifacts remain after
both success and failure. It never installs an app, connects to real remote
hosts, or addresses the user's existing sessions. Private HOME, XDG paths,
config, state, socket, registry, and a dedicated `everyday-ux` session isolate
the fixture. The staged bundle uses the existing
[dev identity machinery](../README.md#running-a-local-build-beside-the-installed-app).
That machinery's documented SDK-global log/window-state limitation still applies.
Both optional server listeners are explicitly ephemeral loopback endpoints;
the server cannot auto-select an overlay listener. SIGTERM runs child cleanup;
a child that ignores termination is killed by its exact process handle and reaped.

## Read the evidence

- Exit **0**: the implemented smoke assertions passed.
- Exit **1**: actual product assertions failed. Read `results.json`,
  `assertions.json`, and the numbered before/after snapshots.
- Exit **2**: an instrument/build/launch/focus failure prevented a verdict.
  Read `infrastructure-error.txt` and the build/app/server logs. Partial
  assertions survive in `assertions.json`; a compilation failure is not RED
  evidence for a product bug.

`provenance.json` records the source commit/diff, build binding, staged bundle,
binary hashes, publisher PID, private server PID/socket, config/state paths,
CLI identity, and linked FFI archive/header hashes. `rust-source.diff` captures
the associated Rust/Cargo edits. Every input is preceded by a live publisher check and an
independent frontmost-PID check. AppKit menu inventory comes from System Events;
rendered widget assertions require the expected semantic role, enabled state,
positive geometry, and invoking window, excluding menu declarations and source fixtures.
Pointer activation resolves the named host accessibility element and sends a
CoreGraphics pointer down/up at its on-screen center. Scrollable actions use a
real CoreGraphics wheel event before the pointer click. No SDK synthetic input
is used.
Missing Accessibility permission produces exit 2 before any input. Grant it to
the responsible hosting application in macOS System Settings before rerunning;
remote launch chains can have a different TCC attribution than the visible terminal.

The shell marker is checked by an independent CLI observer. Saved-machine
fixtures are added through the real CLI registry writer, including six
distinct loopback SSH user destinations and a disabled satellite. No saved
destination is activated. The editor fixture records its actual argv, including
a path containing spaces. Its launch check runs only when the rendered Edit
Configuration action exists.

This is a discovery smoke, not full acceptance of
[the product contract](../docs/specs/phux-2jza/PRODUCT.md). Real remote
transport, minimized-window raising, same-session multiwindow ownership,
lifecycle/resource survival, exhaustive settings transactions, keyboard
remapping, terminal interaction latency, and visual fidelity require their
own journeys. A green menu/discovery assertion does not establish those paths.
The driver makes no screenshot or CoreText fidelity claim; see
[render fidelity](../docs/RENDER_FIDELITY.md).

## Instrument checks (no live app)

```sh
PYTHONDONTWRITEBYTECODE=1 python3 clients/cockpit/scripts/everyday-ux-check.py
```

These tests prove that stale publisher/focus evidence fences input, inherited
destinations cannot escape isolation, and menu/source text cannot pass a
rendered-widget assertion. They do not simulate the product journeys.
