---
audience: agents, contributors
stability: evolving
last-reviewed: 2026-09-23
---

# Native terminal input foundation

**TL;DR.** `input::TerminalInput` is a GPUI `EntityInputHandler` backed by the
existing FFI client registry and an actual runtime `ViewId`. It is an adapter
for the painter to wire, not a completed desktop input product. The fixture is
test-only; shipping builds must not enable `input-fixture`.

## Parent integration

The input commit intentionally does not change the parent's manifest, lockfile,
host registration, or painter. Add `pub mod input;` to the host and these direct
dependencies if absent:

```toml
phux-client-ffi = { path = "../../../crates/phux-client-ffi", default-features = false, features = ["napi"] }
phux-protocol = { path = "../../../crates/phux-protocol" }
unicode-segmentation = "1"

[features]
input-fixture = []
```

The two phux dependencies reuse the shared registry and protocol atoms instead
of duplicating either. `unicode-segmentation` is already in the runtime's graph;
the direct dependency is needed to keep IME UTF-16 ranges on complete graphemes.
Regenerate the host lockfile after merging dependency changes.

For each mounted terminal custom element:

1. Keep one `Entity<TerminalInput>` and one `FocusHandle`. Construct with
   `TerminalInput::new(client_handle, view_id, terminal_id, focus, window)` inside
   `cx.new`. Never share this entity between views or native windows. Recreate
   it when the binding changes; drop it on destroy.
2. Attach `.track_focus(&focus)` to the terminal's `custom_surface`. Native
   focus, not a JS `focused` prop, authorizes input. Focus on a hit-tested press
   before calling `mouse_down`. Retain focus/blur and activation subscriptions;
   call `focus_changed(active, window, cx)` and notify the renderer. Blur must
   cancel the native platform composition as well as this local state.
3. After `Prepared::paint` returns a successful `Observation`, use its actual
   `frame` and `geometry`. Call `presented(&frame, InputMetrics { bounds:
   geometry.bounds, cell_width: geometry.cell_width, line_height:
   geometry.cell_height, cursor_bounds })`. Derive `cursor_bounds` with the
   painter's `geometry.cell_bounds` and the same cursor row/column/width used
   to paint. Do not measure the font again or use JS cell dimensions.
4. In that paint callback, register
   `window.handle_input(&focus, ElementInputHandler::new(geometry.bounds,
   input.clone()), cx)`. Subscribe to input-entity changes to repaint the owning
   surface: `EntityInputHandler` calls `cx.notify()` on composition changes.
   `preedit()` exposes only local marked text and its UTF-16 selection for a
   native overlay. The painter owns shaping that overlay.
5. Route bubbling key events to `key_down`/`key_up`. Stop propagation for
   `Consumed` and for terminal admission errors; allow `Platform` to reach
   native text/menu handling. Do not send a second text event from JS.
   Printable key-down saves metadata; only the platform commit sends its text.
   Named/control/Option-as-Alt keys send on key-down and are consumed there.
6. Route native down/move/up and scroll to the matching methods. Use the
   painter's hitbox and GPUI pointer capture/window listeners so releases and
   selection drags reach the originating view outside its bounds. Respect the
   returned handled boolean. Surface errors and call `cancel()` after rejected
   interaction sequences. The adapter rejects initial hits in grid padding.
7. Bind native Copy/Paste actions to `copy_selection` and `paste_text` (or the
   input handler's `paste`). Command shortcuts otherwise return `Platform`.
   Keep delivery outcomes with the existing sole FFI event owner; the returned
   paste correlation is not delivery confirmation.

`presented` permanently rejects a different connection epoch or replica
stream/bootstrap identity on an already-bound entity. Cancel/discard native
marked text and drop the old entity before creating a fresh one on such a
transition. Merely painting a new replica must never re-arm an old IME callback.
Viewport size changes reject actions until matching geometry is painted.
Initial fixture mounting waits for the real per-terminal `AttachAnswered`, not
only session `Attached`: mounting during initial subscription negotiation can
legitimately encounter another replica generation and become stale immediately.

## Admission and semantics

Every action resolves `phux_client_ffi::napi::initialize().client(handle)`;
there is no second registry, listener, socket, or event drain. Under one
`Client::with_control` closure, admission checks current engine view membership
with `view_replica_info`, matching view publication/terminal and replica,
connection epoch, painted dimensions, actual declared viewer role, and runtime
readiness/delivery fencing, then queues the operation. Paste admission and
journal submission share that lock. Local refusal is safe after unlock and
uses the runtime's existing refusal receipt. Immediate acknowledged outcomes
follow the existing wake contract.

Local selection/copy/scroll require current identity and focus, but do not
require input ownership. Shift at press chooses local selection instead of
application mouse reporting; a started gesture keeps its routing until release.
Shift-wheel uses `ControlPlane::scroll_view` under the same admission lock (the
operation used by `Client::scroll_view`), preserving history routing. Application
mouse uses protocol events and the terminal's current mode. Fractional wheel
deltas accumulate; application reports are bounded to 100 ticks per event.
Clipboard copy uses the runtime's bounded selection API with a 1 MiB byte cap
and its existing formatting-work cap; refusal never copies a truncated string.

The editable text model contains only pending IME text, capped at 4096 UTF-8
bytes. Terminal output and the remote shell buffer are never advertised as an
editable surrounding document. Queries and replacements adjust UTF-16 ranges
to complete graphemes and return adjusted ranges. Commit clears preedit once;
unmark alone cancels it without emission. Candidate bounds are the authoritative
painted cursor rectangle, not an invented width for shaped preedit text.

## Qualification and limits

The isolated fixture uses GPUIX's actual offscreen Metal renderer, the same
shared `DesktopClient`, a real PTY-backed phux server, and a Python raw-input
recorder. Native key/mouse dispatch goes through GPUI. Mark/commit callbacks are
injected into the real `EntityInputHandler`; these are not real OS IME sessions.
The `input-fixture` feature explicitly simulates activation because offscreen
windows are inactive. Production retains the active-window gate.

The pinned GPUI `Keystroke` exposes layout-resolved names and text, not hardware
scan codes, modifier sides, or Num Lock state. The adapter maps available names
to existing libghostty-compatible protocol atoms, including function/navigation
keys, press/repeat/release and Caps Lock. Full layout-independent physical keys
and keypad fidelity are **not qualified**. Option-as-Alt defaults off and can be
configured with `set_option_as_alt`; native IME-first Option behavior still needs
real-platform testing.

Manual acceptance remains: Japanese/Chinese/Korean/dead-key OS IMEs, candidate
placement under scale changes, AppKit activation and focus loss mid-composition,
native menu ownership, clipboard integration with other applications, VoiceOver
and the terminal accessibility tree, and hardware keyboard layouts/keypads.
Selection autoscroll, search UI, link activation, and full accessibility are not
implemented by this foundation. The parent must review and qualify the combined
painter/input wiring; this slice does not close first-release input acceptance.

## Reproduce the fixture

Temporarily include `tests/native/input_fixture.rs` from host `src/lib.rs` with
`#[path = "../../tests/native/input_fixture.rs"]` and
`#[cfg(feature = "input-fixture")]`. In the existing single extension installer,
call `input_fixture::install(registry)` behind the same feature.

For the isolated launcher, temporary host manifest wiring is:

```toml
[dev-dependencies]
phux-server-testkit = { path = "../../../crates/phux-server-testkit" }
tempfile = "3"
tokio = { version = "1", features = ["process", "time"] }
portable-pty = "0.9"

[[example]]
name = "input_server"
path = "../tests/native/input_server.rs"
```

Use the verified pinned GPUIX source plus patch 0001, source
`scripts/lib/apple-toolchain-env.sh` and `scripts/lib/dev-toolchain.sh`, and set
`RUSTUP_TOOLCHAIN="$RUST_CHANNEL"`. Set a worktree-private `CARGO_TARGET_DIR`.
Build the host and `input_server` with `--features input-fixture`, copy the host
dylib to a `.node` file, then run `input_server /absolute/path/input.node`.
Restore temporary host wiring after testing. Debug builds qualify behavior only;
they do not qualify release performance or packaging.

## Validation receipt

Validated in the isolated `phux-desktop-input` worktree against the initial
shared-host base plus the parent's runtime fixes through `4d8dea890`. Those
runtime commits are prerequisites, not part of the input handoff patch.

- `bash scripts/doctor.sh desktop`: zero problems.
- Source verifier: GPUIX `6d5e6887`, Zed `81c99f81`, pinned lock hashes and
  patch 0001 verified in this worktree's own ignored source checkout.
- `cargo test --manifest-path clients/desktop/native/Cargo.toml --lib input::`:
  four passing range/grapheme/key/Option-policy unit tests.
- `cargo clippy --manifest-path clients/desktop/native/Cargo.toml --lib
  --all-features -- -D warnings`: passed, including test-fixture feature code.
  The broader `--all-targets` attempt also compiled the pre-existing probe as
  a Rust test and failed its NAPI-only `HostProbeCounts`/`desktop_host_probe_counts`
  dead-code diagnostics. That is not claimed as a pass.
- Test host plus isolated `input_server` built; the offscreen Metal/real-PTY
  fixture passed twice after adding the initial subscription-confirmation
  barrier. It asserts exact printable/composition bytes, Kitty repeat/release,
  mouse reports, Shift local selection and independent scrolling, paste
  Delivered/unsafe Refused receipts, inactive-window rejection, an actual
  runtime resync followed by rejected old IME commit, destroyed view, and closed
  handle. Test-only activation simulation is described above.
- `rustfmt --edition 2024 --check` on the new Rust files and
  `bash scripts/check-docs.sh`: passed.
- Lizard with threshold 10 found no functions over the threshold. The only
  first-draft hotspot was `scroll`: CC 12 to 9, extracting `report_scroll`
  (CC 5). New-function baseline is otherwise not applicable; maximum final CC
  is 9. Meaningful behavior was verified by the GPU/PTY fixture after extraction.

Local evidence is under `clients/desktop/.cache/input-{tests,clippy,build,smoke,
smoke-repeat,complexity,docs}.log` (complexity uses `.txt`), with artifacts in
the private `.cache/input-target`. Independent nested review was unavailable
because the harness subagent depth limit is one; the integrating parent still
owns a fresh-context review of the combined painter and input wiring.
