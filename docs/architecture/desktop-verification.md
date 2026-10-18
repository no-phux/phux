---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-10-14
---

# Desktop verification

**TL;DR.** Desktop acceptance requires a real GPUI frame, an isolated
same-checkout server, and evidence written by that running app. A missing PNG,
an unpresented layout, or a harness-written ok flag is a failure. Headless
models cannot prove GPU, IME, or accessibility behavior.

[ADR-0165](../adr/0165-gpui-desktop-replaces-the-solid-host.md) is the host
decision. [Desktop architecture](./desktop.md) owns the seams. The
[consumer contract](../consumers/desktop.md) owns user-visible behavior. This
page is the verification contract, not a record of passing tests.

## Evidence receipt

Every GPU gate records revision, command, exit status, socket, server
incarnation, and artifact paths. Native receipts add the GPUI snapshot
revision, OS/GPU/display, window identity, and the presented generation from
`layout.json`. Tests use an isolated HOME, XDG directories, socket, and
profile. They never attach to `/tmp/phux-$USER/phux.sock`,
`/run/user/<uid>/phux/phux.sock`, or an installed `phux`. Report headless,
GPU, accessibility, performance, and package evidence separately. Missing GPU
access is blocked, not passed.

## Package and smoke

| Check | Required evidence |
|---|---|
| Build | `clients/desktop/native/target/release/phux-desktop` from `just desktop-native-build`. No `.node` addon and no JS bundle. |
| Package | `Phux.app` runs that binary, ships Paper Mono and the icon, and is ad-hoc signed. `PhuxDesktopHost` is `gpui`. |
| Linux package | The tarball's `phux-desktop/bin/phux-desktop` runs with the packaged metadata, desktop entry, icon and license notices. Native x64 and ARM64 lanes use Ubuntu 24.04. |
| CLI boundary | Smoke and perf pass `--phux` explicitly. The path is not an installed CLI. The app does not bundle or upgrade `phux`. |
| Presentation | `--evidence` writes `presented.png` from the drawable-presented callback, `layout.json` with `presented: true` and a positive generation, and `interaction.json` for that same generation. |
| OCR | macOS Vision reads the PNG and finds the live shell marker. The server snapshot alone is not enough. |
| Survival | SIGKILL and SIGTERM of the app leave the same server pid and the same shell pid. `--smoke` exiting 0 is a clean exit, not that crash proof. |
| Perf | `--evidence` without `--smoke` appends `perf.jsonl` with host `metrics` for `desktop.acquire`, `desktop.prepare`, `desktop.paint`, and `desktop.present`. A fabricated line is not a measurement. |

`just desktop-framework-check` is the packaged form of this gate. The release
lane runs the same smoke on the extracted archive before upload.

Linux runs `clients/desktop/scripts/smoke-linux.ts` on the extracted tarball
under Xvfb and lavapipe. It records submitted-frame evidence, reads the live
terminal marker with Tesseract, and checks that the same shell and server
survive SIGKILL/SIGTERM and relaunch. The receipt's `presented: false` is
intentional: successful wgpu submission is not a physical-display presentation
receipt, and it cannot clear an unknown-delivery fence. The 13 offscreen
pixel fixtures separately cover glyph placement, symbol fallback, colors,
selection, blocks, streaming, and scroll. Neither lane establishes native IME,
screen-reader, physical GPU/performance or Wayland compositor qualification.

## Product journeys still required

These are not implied by the host cutover:

| Surface | Still required |
|---|---|
| Duplicate views | Two panes or windows of one terminal, independent scroll/selection/search, one size owner, close of one view leaves the other. |
| Persistence | Same-incarnation restore, changed-incarnation refusal, malformed and newer files left unwritten. |
| Connections | Startup error stays in the window. Reconnect does not invent a second lifecycle. Remote hosts are not built. |
| Settings and Ghostty | Effective value, font, palette, keybinds, and the quick-terminal chord. `reset`, `vt`, and `html` copy formats remain unsupported. |
| Agents | Attached-mode badges and attention, terminal/conversation switching, same-terminal acknowledged composer delivery, once-per-working-turn Stop, and read-only ended streams; attached approvals remain terminal-owned. ACP launch, typed exact-session commands, structured dialogs, same provider session across viewers, honest admission receipts, and read-only disconnection/end. |
| Accessibility | A real screen-reader and IME journey. Manual observation is required; do not mark it passed from a unit test. |
| Linux | Physical GPU/performance, compositor-specific Wayland, native IME and screen-reader journeys remain separate from X11/lavapipe package qualification. |

### Native conversation gates

The ignored attached-mode journey
`agent_stream_enables_native_face_without_restart_and_keeps_drafts` opens a
private server and exercises dynamic stream discovery, stable upserts, face
switches, the Kit multiline field, acknowledged paste+Return, attached Stop, permission
refusal, and stream closure. Run it with `cargo test --locked --manifest-path
clients/desktop/native/Cargo.toml --lib agent_stream_enables_native_face -- --ignored`.
It requires `PHUX_DESKTOP_JOURNEY_TEST_BIN` naming a checkout-built CLI and
`XDG_STATE_HOME` under disposable temporary storage or `clients/desktop/.cache`.

For an actual offscreen GPUI frame, run `cargo test --locked --manifest-path
clients/desktop/native/Cargo.toml --features terminal-fixtures --test pixels
app::conversation::native_face_is_rendered -- --ignored --nocapture` with the same
environment. It renders the production Desk and native prose/thinking/tool
rows through Metal on the main thread, using records from its private server.
`PHUX_DESKTOP_PIXEL_EVIDENCE` optionally names the captured conversation PNG.
This retained fixture does not generate SDK question scenes or exercise provider
transport/admission.

Separate qualification exercised all seven native question kinds—confirm,
select, input, editor, multiple questions, permission and form—through the actual
Desk/Kit Metal renderer, and each resulting PNG was inspected. The temporary
injected-scene loop and actual-SDK GPUI journey were removed after qualification;
they are not shipped regression tests or outputs promised by the retained
command above. Offscreen rendering requires neither screen-capture permission
nor a frontmost window; it does not qualify live presentation, accessibility,
IME or cross-view provider control.

ACP compatibility checks use isolated credentials-free `initialize` probes.
The real CLI/server regression peer covers protocol events rather than terminal
output. These checks do not qualify live GPUI, AX/IME, mobile rendering, or
provider model behavior. Previous provider-specific SDK experiments do not
qualify the replacement ACP seam.

ACP acceptance additionally requires an isolated same-checkout server and
the genuine installed/pinned provider SDK. Launch from **New ACP Agent** and verify the
returned guardian/AgentSession/activation; desktop and mobile must control that
same provider-native session. Verify multiline prompt, actual-turn interrupt,
confirm/select/input/editor/multi-question/permission/form replies, first responder
across viewers, request closure while editing, retained drafts/scroll, viewer
detach and daemon reconnect. Capture queued versus provider-admitted outcomes,
including refusal and ambiguous disconnect without resend or terminal fallback.
Missing runtime and license setup must be actionable and must not install or
approve anything. Repeat configured supported integrations without treating a
terminal-attached hook as SDK evidence. Scene fixtures and unit tests cannot close
this acceptance gate, and the root implementation remains draft until integration
verification and shipping are recorded.


## Tooling gate

`just desktop-check` is the CI gate. It typechecks and lints the script
toolchain, runs the qualification-guard tests, and compares the desktop host
lock with the root lock. It does not launch GPUI. A green check is not package
or presentation evidence.

## Status

| Target | Current gap |
|---|---|
| Packaged smoke | The harness rejects fake evidence. A presented frame on this machine is still an execution record, not a claim in this document. |
| Product journeys | Specified above. ACP launch/dialog implementation is not itself a real-provider, cross-view, GPU or shipping record. Notarization and Linux physical-display/IME/accessibility remain separate gates. |
| Performance | The bench reads host histograms. A number from a loaded machine is observational until a hardware-tagged baseline exists. |
