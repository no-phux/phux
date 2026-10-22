# Key bindings

A key binding maps a key to an **action**, such as splitting a pane or
switching sessions. You change bindings in the
[configuration file](./config.md), under `[keybindings]`.

```toml
[keybindings.prefix-table]
"v" = { action = "split-pane", direction = "vertical" }
"r" = "reload-config"

[keybindings.global]
"M-Enter" = "toggle-zoom"
```

Run `phux config reload` after changing the file for the bindings to take
effect.

## The prefix

Most phux keys are two presses: the **prefix**, <kbd>C-a</kbd>, then a
key. <kbd>C-a</kbd> <kbd>z</kbd> zooms the focused pane, and
<kbd>C-a</kbd> <kbd>d</kbd> detaches.

Press the prefix and hesitate, and after 400 milliseconds a popup lists
every key you can press next, your own bindings included. The popup never
gets in the way: whatever you press runs as if it weren't there, and
<kbd>Esc</kbd> cancels the prefix. Change the delay or turn the popup off:

```toml
[keybindings]
which-key = true
which-key-delay-ms = 400
```

After the prefix, a key with no binding goes to the program in the pane;
only the prefix itself is swallowed. So <kbd>C-a</kbd> <kbd>C-a</kbd>
sends one literal <kbd>C-a</kbd> to the program, which is how you reach
readline's beginning-of-line.

[Default key bindings](./reference/keys.md) lists every key phux binds out
of the box, grouped the way the popup groups them.

## Binding a key

Bindings live in two tables:

| Table | Fires |
| --- | --- |
| `[keybindings.prefix-table]` | After the prefix. `"z"` here is <kbd>C-a</kbd> <kbd>z</kbd>. |
| `[keybindings.global]` | On its own, with no prefix. phux ships none. |

A binding's value is an action name, or an inline table with the action
and its arguments:

```toml
[keybindings.prefix-table]
"r" = "reload-config"
"R" = { action = "resize-pane", direction = "right", amount = 10 }
"," = { action = "rename-window", name = "build" }
```

Some actions require arguments and some take optional ones. `resize-pane`
needs a `direction` and an `amount`; `rename-window` opens a prompt when
you leave out `name` and renames straight away when you give one.

**A global binding takes the key away from every program.** A global
<kbd>M-Enter</kbd> never reaches your editor or shell again. Pick global
keys that your programs don't use.

## Finding actions

- Press <kbd>C-a</kbd> <kbd>Space</kbd> to open the command palette. It
  lists every action you can run, searchable, with the key bound to it.
- [Actions](./reference/actions.md) lists every action name with its
  arguments.
- `phux config keys` prints every binding in effect (see
  [Inspecting bindings](#inspecting-bindings)).

The actions keep tmux's word "window" for a tab: `new-window` opens a tab,
`select-window` jumps to one.

## Writing keys

A key is written as zero or more modifiers followed by one key, joined by
`-`. `C-a` is Ctrl+A and `C-M-x` is Ctrl+Alt+X.

| Modifier | Means |
| --- | --- |
| `C-` | Ctrl |
| `M-` | Alt (Meta) |
| `A-` | Alt, the same as `M-` |
| `S-` | Shift |

Modifier letters are uppercase. `Ctrl-a` is not a key; write `C-a`.

The key after the modifiers is one of:

- A letter, digit, or ASCII punctuation character: `a`, `7`, `[`, `|`.
- A named key: `Tab`, `BackTab`, `Enter`, `Esc` (or `Escape`), `Space`,
  `Backspace`, `Delete`, `Insert`, `Up`, `Down`, `Left`, `Right`, `Home`,
  `End`, `PageUp`, `PageDown`, and `F1` through `F24`. Names are
  case-sensitive, so `enter` is not a key.

A key names the physical key you press, not the character it types:

- **A capital letter means Shift.** `"X"` and `"S-x"` are the same key,
  and so are `"C-X"` and `"C-S-x"`.
- **Punctuation is a key on a US layout.** `"|"` is Shift plus the
  backslash key, the same as `"S-\\"`, and `"?"` is Shift plus `/`. On
  another layout, write the shifted key with an explicit `S-`.
- `BackTab` is Shift+Tab.

A binding can also be a **sequence**: several keys pressed one after
another, separated by spaces. In the prefix table, `"g s"` is
<kbd>C-a</kbd> <kbd>g</kbd> <kbd>s</kbd>:

```toml
[keybindings.prefix-table]
"g s" = "session-picker"
"g w" = "window-picker"
```

A key either runs an action or starts a sequence; it can't do both,
because phux could never tell which one you meant. Binding both `"g"` and
`"g s"` is an error.

Quote keys that TOML would read differently: `'"' = ...` binds the double
quote, `"\\" = ...` the backslash.

## Replacing a default

To change what a default key does, bind it. There is no need to unbind it
first:

```toml
[keybindings.prefix-table]
# C-a o goes back to the last pane instead of cycling through them
"o" = "last-pane"
```

Your binding replaces the shipped one for that key whole, arguments
included, whatever spelling you use: `"S-x"` in your file replaces the
shipped `"X"`.

Within one file, two spellings of the same key conflict. `"X"` and `"S-x"`
in the same file is an error, and `phux config check` reports it.

## Removing a binding

Set a key to `"none"` to remove its binding, a shipped default included:

```toml
[keybindings.prefix-table]
"x" = "none"

[keybindings.global]
"M-Enter" = "none"
```

**Unbinding a key gives it back to the program.** An unbound global key
goes straight to the program in the pane. An unbound prefix-table key
goes to the program after the prefix, so <kbd>C-a</kbd> <kbd>x</kbd> now
types an `x`.

## Changing the prefix

```toml
[keybindings]
prefix = "C-b"
```

Every prefix-table key moves with it: <kbd>C-b</kbd> <kbd>z</kbd> now
zooms. The popup, the palette, and `phux config keys` all show the new
prefix.

A global binding on exactly the prefix key can never fire, because the
prefix always wins. phux drops it and `phux config check` reports it. A
prefix that does not parse falls back to <kbd>C-a</kbd>.

## Coming from tmux

This file keeps tmux's prefix and brings back a few tmux habits:

```toml
[keybindings]
prefix = "C-b"

[keybindings.prefix-table]
# tmux's & closes the tab after asking (phux ships this on X)
"&" = "kill-window"

# tmux's ( and ) step through sessions (phux ships N and P)
"(" = "previous-session"
")" = "next-session"

# r to reload, the way many .tmux.conf files bind source-file
"r" = "reload-config"
```

Splits (<kbd>"</kbd> <kbd>%</kbd>), new tab (<kbd>c</kbd>), next and
previous tab (<kbd>n</kbd> <kbd>p</kbd>), copy mode (<kbd>[</kbd>), and
detach (<kbd>d</kbd>) already match tmux. One difference to know:
phux's <kbd>x</kbd> closes the pane without asking. The getting started
guide lists [what else differs from tmux](./getting-started.md#coming-from-tmux).

## Inspecting bindings

`phux config keys` prints every binding in effect: the shipped defaults,
your file and the files it extends, and keys from enabled plugins, merged
exactly as phux merges them. Prefix-table keys are shown after the prefix:

```text
$ phux config keys split-pane
prefix  C-a  (defaults)

KEY    ACTION      ARGS                  SOURCE
C-a "  split-pane  direction=horizontal  defaults
C-a %  split-pane  direction=vertical    defaults
C-a -  split-pane  direction=horizontal  defaults
C-a |  split-pane  direction=vertical    defaults
```

Give it an action name to see only that action's keys, or none to see
everything. **SOURCE** is `defaults`, the name of the file that set the
binding (`config.toml`, or a file you extend), or `plugin <id>`. The first
line shows the prefix and where it came from.

Bindings phux would skip are listed as warnings after the table, so a
binding that does nothing never hides:

```text
phux: warn: skipped keybinding "q": unknown action `kill-pain` (did you mean `kill-pane`?)
```

`--json` prints the same as a stable JSON document.

## Debugging

`phux config check` reports every binding problem with its key path. The
same problems appear as `phux config keys` warnings:

```text
$ phux config check
keybindings.prefix: bad chord: unknown key name: Ctrl-a
keybindings.prefix-table.q: unknown name: unknown action `kill-pain` (did you mean `kill-pane`?)
keybindings.prefix-table."g s": bad chord: ambiguous prefix: 'g s' is both a binding and a prefix
3 problems
```

| Finding | Cause | Fix |
| --- | --- | --- |
| `unknown key name` | The key isn't a character or named key, often a lowercase name or `Ctrl-`. | Use the syntax in [Writing keys](#writing-keys). |
| `invalid chord syntax at N` | A malformed key, such as a trailing `-`. | Fix the character at offset N. |
| `unknown action` | A misspelled action. The finding suggests the closest one. | Check [Actions](./reference/actions.md). |
| `ambiguous prefix` on a sequence | One key both runs an action and starts a sequence (`"g"` and `"g s"`). | Remove one of them. |
| `ambiguous prefix` on a plain key | Two spellings of one key in the same file (`"X"` and `"S-x"`). | Keep one spelling. |
| `ambiguous prefix` in `global` | A global binding on the prefix key. | Bind another key, or change the prefix. |

A bad binding disables only itself when you attach; every other binding
still works. `phux config reload` is stricter: it refuses a file with any
bad binding and keeps your current bindings, so fix the finding and reload
again.

If a key seems to do nothing, run `phux config keys` and look for it. A
plugin key loses to yours when both bind the same key, and a binding you
removed with `"none"` is absent from the table.
