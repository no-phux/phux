---
audience: contributors, agents
stability: scratch
last-reviewed: 2026-06-23
---
# Launch Polish QA

**TL;DR.** This note records the launch-polish evidence for the README
logo/demo assets and the TUI chrome smoke. The patch replaces front-door
placeholders with real assets, then verifies docs, asset dimensions, build
health, and real tmux-driven help/copy-mode/command-palette surfaces.

## Scope

- Replaced the README logo placeholder with `docs/assets/logo.svg`.
- Added `docs/assets/logo.png` as a raster export for surfaces that do not
  render SVG.
- Replaced the README demo placeholder with `docs/assets/demo.gif`.
- Added root `DESIGN.md` so future visual assets and TUI polish have a shared
  color/type/layout contract.
- Updated `docs/demo.md` so the runbook reflects that the README now points at
  a committed demo asset.

## Asset Checks

Commands:

```sh
sips -g pixelWidth -g pixelHeight -g hasAlpha docs/assets/logo.png
file docs/assets/demo.gif docs/assets/logo.svg docs/assets/logo.png
```

Observed:

```text
docs/assets/logo.png
  pixelWidth: 960
  pixelHeight: 260
  hasAlpha: yes
docs/assets/demo.gif: GIF image data, version 89a, 960 x 540
docs/assets/logo.svg: SVG Scalable Vector Graphics image
docs/assets/logo.png: PNG image data, 960 x 260, 8-bit/color RGBA, non-interlaced
```

`docs/assets/demo.gif` is 49 KB. `docs/assets/logo.png` is 175 KB.

## Verification

Docs:

```sh
bash scripts/check-docs.sh
```

Result:

```text
checked 88 files, 0 violations
```

Build:

```sh
nix develop -c cargo build --bin phux
```

Result:

```text
Finished `dev` profile [optimized + debuginfo] target(s) in 50.13s
```

Plain `cargo build --bin phux` failed outside the devshell because
`libghostty-vt-sys` could not find `zig`. The repo's supported path is the Nix
devshell, which provides `zig_0_15`.

## Real TUI Surface

Channel: isolated tmux server plus isolated phux Unix socket.

Surface setup:

```sh
tmux -L phux-polish-qa-clean new-session -d -s polish -x 100 -y 32 \
  "env HOME=/tmp/phux-polish-qa-clean/home \
       ZDOTDIR=/tmp/phux-polish-qa-clean/zdot \
       SHELL=/bin/sh \
       RUST_LOG=info \
       /private/tmp/phux-launch-polish/target/debug/phux \
       attach --socket /tmp/phux-polish-qa-clean/phux.sock polish \
       2>/tmp/phux-polish-qa-clean/client.log"
```

Captured states:

- `01-attach.txt`: first attach and status bar.
- `02-status-command.txt`: command output plus status bar.
- `03-help-overlay.txt`: `C-a ?` help overlay.
- `04-copy-mode.txt`: `C-a [` copy-mode status strip.
- `05-command-palette.txt`: `C-a :` command palette.

Cleanup receipt:

```text
tmux -L phux-polish-qa-clean kill-server; pkill phux server socket; rm -f socket
socket-clean
```

## Visual QA

Tool:

```sh
bun /Users/phall/.codex/plugins/cache/sisyphuslabs/omo/4.12.1/skills/visual-qa/scripts/cli.ts tui-check <capture> --cols 100
```

Results:

```json
{"capture":"03-help-overlay.txt","maxWidth":100,"overflowLines":[],"borderMisaligned":false,"wideCharColumns":[]}
{"capture":"04-copy-mode.txt","maxWidth":67,"overflowLines":[],"borderMisaligned":false,"wideCharColumns":[]}
{"capture":"05-command-palette.txt","maxWidth":100,"overflowLines":[],"borderMisaligned":false,"wideCharColumns":[]}
```

Representative help overlay capture:

```text
               ┌──────────────────────────── phux help ─────────────────────────────┐
               │Prefix bindings (C-a)                                               │
               │C-a "    split-pane(direction=horizontal)                           │
               │C-a $    rename-session                                             │
               │C-a %    split-pane(direction=vertical)                             │
               │C-a ,    rename-window                                              │
               │C-a :    command-palette                                            │
               │C-a ;    previous-pane                                              │
               │C-a ?    show-help                                                  │
               │C-a C    new-session                                                │
               │C-a H    resize-pane(amount=5, direction=left)                      │
               │C-a J    resize-pane(amount=5, direction=down)                      │
               │C-a K    resize-pane(amount=5, direction=up)                        │
               │C-a L    resize-pane(amount=5, direction=right)                     │
               │C-a W    take-input                                                 │
               │C-a X    kill-window                                                │
               │C-a [    copy-mode                                                  │
               │C-a a    session-picker                                             │
               │C-a b    toggle-sidebar                                             │
               │C-a c    new-window                                                 │
               │C-a d    detach                                                     │
               └────────────────────────────────────────────────────────────────────┘
```

Representative copy-mode capture:

```text
 copy-mode | 1 cell(s) | arrows/PgUp/PgDn scroll | Enter copy | Esc
```

Representative command palette capture:

```text
                    ┌──────────────────── command palette ─────────────────────┐
                    │>                                                         │
                    │                                                          │
                    │Pane                                                      │
                    │  Split the focused pane side-by-side (vertical divider) C│
                    │  Close the focused pane                             C-a x│
                    │  Move focus to the pane on the left                 C-a h│
                    │  Grow the focused pane to the left                  C-a H│
                    │  Cycle focus to the next pane                       C-a o│
                    │  Cycle focus to the previous pane                   C-a ;│
                    │  Zoom the focused pane to fill the window (toggle)  C-a z│
                    │Window                                                    │
                    │  Open a new window                                  C-a c│
                    │  Close the active window and all its panes          C-a X│
                    │  Switch to the next window                          C-a n│
                    └──────────────────────────────────────────────────────────┘
```
