# Default key bindings

Every key phux binds out of the box.

<!--
GENERATED FILE - do not edit. A unit test byte-compares this page
against `phux gen-reference-docs` output and fails on any drift, so
hand edits do not survive. Regenerate with `just docs-gen`.
-->

Press the prefix, <kbd>C-a</kbd>, then one of the keys below. Hesitate after the prefix and phux shows this same list in a popup. Every key can be rebound or unbound in `[keybindings.prefix-table]`; see [Key bindings](../keys.md). `phux config keys` prints the bindings in effect, your changes included.

A capital letter means Shift: <kbd>X</kbd> is <kbd>Shift</kbd>+<kbd>x</kbd>. Where two keys share a row, either one works.

## Pane

| Keys | Does | Action |
|---|---|---|
| <kbd>"</kbd> <kbd>-</kbd> | Split below | `split-pane` |
| <kbd>%</kbd> <kbd>\|</kbd> | Split right | `split-pane` |
| <kbd>x</kbd> | Kill pane | `kill-pane` |
| <kbd>h</kbd> <kbd>j</kbd> <kbd>k</kbd> <kbd>l</kbd> | Focus left, down, up, right | `focus-direction` |
| <kbd>H</kbd> <kbd>J</kbd> <kbd>K</kbd> <kbd>L</kbd> | Resize left, down, up, right | `resize-pane` |
| <kbd>o</kbd> | Next pane | `next-pane` |
| <kbd>;</kbd> | Previous pane | `previous-pane` |
| <kbd>=</kbd> | Last pane | `last-pane` |
| <kbd>z</kbd> | Zoom pane | `toggle-zoom` |
| <kbd>[</kbd> | Copy mode | `copy-mode` |
| <kbd>F</kbd> | Insert path… | `find-path` |
| <kbd>W</kbd> | Take the wheel | `take-input` |
| <kbd>g</kbd> | Give back the wheel | `give-input` |
| <kbd>f</kbd> | Freeze pane | `signal-terminal` |
| <kbd>u</kbd> | Resume pane | `signal-terminal` |

## Tab

| Keys | Does | Action |
|---|---|---|
| <kbd>c</kbd> | New tab | `new-window` |
| <kbd>X</kbd> | Kill tab… | `kill-window` |
| <kbd>0</kbd>–<kbd>9</kbd> | Go to tab | `select-window` |
| <kbd>Tab</kbd> <kbd>n</kbd> | Next tab | `next-window` |
| <kbd>p</kbd> | Previous tab | `previous-window` |
| <kbd>w</kbd> | Go to tab… | `window-picker` |
| <kbd>,</kbd> | Rename tab… | `rename-window` |
| <kbd>&lt;</kbd> | Move tab left | `move-window` |
| <kbd>&gt;</kbd> | Move tab right | `move-window` |
| <kbd>G</kbd> | New tab in folder… | `go-to-directory` |

## Session

| Keys | Does | Action |
|---|---|---|
| <kbd>C</kbd> | New session… | `new-session` |
| <kbd>s</kbd> | Go to session… | `session-picker` |
| <kbd>N</kbd> | Next session | `next-session` |
| <kbd>P</kbd> | Previous session | `previous-session` |
| <kbd>)</kbd> | Last session | `last-session` |
| <kbd>$</kbd> | Rename session… | `rename-session` |
| <kbd>d</kbd> | Detach | `detach` |

## phux

| Keys | Does | Action |
|---|---|---|
| <kbd>:</kbd> <kbd>?</kbd> <kbd>Space</kbd> | Command palette | `command-palette` |
| <kbd>A</kbd> | Agents | `agent-fleet` |
| <kbd>q</kbd> | Next waiting pane | `next-attention` |
| <kbd>Q</kbd> | Back where you were | `return-from-attention` |
| <kbd>b</kbd> | Toggle sidebar | `toggle-sidebar` |
| <kbd>S</kbd> | Settings | `settings` |
| <kbd>B</kbd> | Report a bug | `report-bug` |
| <kbd>a</kbd> | Cycle agents filter | `agents-filter` |
| <kbd>O</kbd> | Organize everything | `organize` |

## Without the prefix

No global bindings ship, so every key without the prefix goes to the program in the pane. Add your own under `[keybindings.global]`.
