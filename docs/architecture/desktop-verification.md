---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-10-13
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
| CLI boundary | Smoke and perf pass `--phux` explicitly. The path is not an installed CLI. The app does not bundle or upgrade `phux`. |
| Presentation | `--evidence` writes `presented.png` from the drawable-presented callback, `layout.json` with `presented: true` and a positive generation, and `interaction.json` for that same generation. |
| OCR | macOS Vision reads the PNG and finds the live shell marker. The server snapshot alone is not enough. |
| Survival | SIGKILL and SIGTERM of the app leave the same server pid and the same shell pid. `--smoke` exiting 0 is a clean exit, not that crash proof. |
| Perf | `--evidence` without `--smoke` appends `perf.jsonl` with host `metrics` for `desktop.acquire`, `desktop.prepare`, `desktop.paint`, and `desktop.present`. A fabricated line is not a measurement. |

`just desktop-framework-check` is the packaged form of this gate. The release
lane runs the same smoke on the extracted archive before upload.

## Product journeys still required

These are not implied by the host cutover:

| Surface | Still required |
|---|---|
| Duplicate views | Two panes or windows of one terminal, independent scroll/selection/search, one size owner, close of one view leaves the other. |
| Persistence | Same-incarnation restore, changed-incarnation refusal, malformed and newer files left unwritten. |
| Connections | Startup error stays in the window. Reconnect does not invent a second lifecycle. Remote hosts are not built. |
| Settings and Ghostty | Effective value, font, palette, keybinds, and the quick-terminal chord. `reset`, `vt`, and `html` copy formats remain unsupported. |
| Agents | Badges and attention where a terminal already exposes them. Approvals are not built. |
| Accessibility | A real screen-reader and IME journey. Manual observation is required; do not mark it passed from a unit test. |
| Linux | A later platform matrix. macOS evidence does not qualify it. |

## Tooling gate

`just desktop-check` is the CI gate. It typechecks and lints the script
toolchain, runs the qualification-guard tests, and compares the desktop host
lock with the root lock. It does not launch GPUI. A green check is not package
or presentation evidence.

## Status

| Target | Current gap |
|---|---|
| Packaged smoke | The harness rejects fake evidence. A presented frame on this machine is still an execution record, not a claim in this document. |
| Product journeys | Specified above. Projects/worktree grouping, remote hosts, approvals, notarization, and Linux are not shipped. |
| Performance | The bench reads host histograms. A number from a loaded machine is observational until a hardware-tagged baseline exists. |
