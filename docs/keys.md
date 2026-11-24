---
group: customize
order: 1
---

# Key bindings

How to bind, rebind, and unbind keys under `[keybindings]` in your
[config file](./config.md). The shipped keys are in
[Default key bindings](./reference/keys.md); every action and its arguments
are in [Actions](./reference/actions.md).

```toml
[keybindings.prefix-table]
"v" = { action = "split-pane", direction = "vertical" }
"r" = "reload-config"

[keybindings.global]
"M-Enter" = "toggle-zoom"
```

Bindings apply on `phux config reload`.

## Tables

| Table | Fires |
| --- | --- |
| `[keybindings.prefix-table]` | After the prefix (<kbd>C-a</kbd>). `"z"` is <kbd>C-a</kbd> <kbd>z</kbd>. |
| `[keybindings.global]` | On its own. phux ships none. |
| `[keybindings.modes.<name>]` | On its own, while that [key mode](#key-modes) is on. |

A value is an action name, or an inline table with the action and its
arguments. A plugin's action is `plugin-action`:

```toml
[keybindings.prefix-table]
"R" = { action = "resize-pane", direction = "right", amount = 10 }
"g" = { action = "popup", command = "lazygit" }
"Y" = { action = "plugin-action", plugin = "example.pane-tools", id = "summarize" }
```

After the prefix, an unbound key goes to the program in the pane, and the
prefix pressed twice sends one literal prefix. **A global binding takes the
key away from every program**, so pick keys your programs don't use.

Hesitate after the prefix and a popup lists every key you can press next
(`which-key`, `which-key-delay-ms`). <kbd>C-a</kbd> <kbd>Space</kbd> opens the
command palette, which runs any action and shows its key.

## Writing keys

Zero or more modifiers then one key, joined by `-`: `C-` Ctrl, `M-` or `A-`
Alt, `S-` Shift. `C-M-x` is Ctrl+Alt+X. Modifiers are uppercase; `Ctrl-a` is
not a key.

The key is a character (`a`, `7`, `[`) or a case-sensitive name: `Tab`,
`BackTab`, `Enter`, `Esc`, `Space`, `Backspace`, `Delete`, `Insert`, arrows
(`Up` ...), `Home`, `End`, `PageUp`, `PageDown`, `F1` to `F24`.

A key names the physical key, not the character it types. A capital letter
means Shift (`"X"` is `"S-x"`), and punctuation assumes a US layout (`"|"` is
`"S-\\"`); on another layout write `S-` explicitly. In TOML, quote `'"'` and
`"\\"`.

A space-separated key is a **sequence**: `"t s"` in the prefix table is
<kbd>C-a</kbd> <kbd>t</kbd> <kbd>s</kbd>. A key either runs an action or
starts a sequence, never both, so the first key must be free: unbind the
shipped `"g"` before binding `"g s"`.

## Replacing and removing

Bind a key to replace what it does; no unbind first. Your binding replaces
the shipped one whole, whatever spelling either uses (`"S-x"` replaces the
shipped `"X"`). Two spellings of one key in the same file are an error.

`"none"` removes a binding, shipped or plugin, and gives the key back to the
program:

```toml
[keybindings.prefix-table]
"x" = "none"
```

A removed key stays removed even when a plugin's manifest `keys` names it.

To build a keymap from nothing, set `defaults = false` under `[keybindings]`.
Only your files' bindings apply, in every table and mode. The shipped
`command-palette` keys stay as the way out until you bind `command-palette`
yourself.

## Changing the prefix

```toml
[keybindings]
prefix = "C-b"
```

Every prefix-table key moves with it. A global binding on the prefix key can
never fire, so phux drops it and `phux config check` reports it.

## Key modes

A key mode makes a set of keys work without the prefix until you leave it.
The shipped resize mode (<kbd>C-a</kbd> <kbd>R</kbd>) is one. The status bar
shows the active mode's name.

```toml
[keybindings.prefix-table]
"v" = { action = "enter-mode", name = "views" }

[keybindings.modes.views]
"n" = "next-window"
"p" = "previous-window"
"q" = "exit-mode"

[keybindings.mode-options.views]
exclusive = true
```

- `exit-mode` leaves; so does <kbd>Esc</kbd> unless the mode binds it.
  `once = true` on `enter-mode` leaves after one key.
- A key the mode doesn't bind behaves as outside the mode, unless the mode is
  `exclusive`, which ignores it so nothing types into the pane.
- One mode at a time. Mode tables layer like the others: `"none"` removes a
  key from that mode only.

### Copy-mode keys

Copy mode's keys are vi's. A mode named `copy-mode` maps your keys onto the
built-in ones with `copy-mode-key`, and `clear-selection` drops the
selection:

```toml
[keybindings.modes.copy-mode]
"C-n" = { action = "copy-mode-key", key = "j" }
"C-p" = { action = "copy-mode-key", key = "k" }
"M-w" = { action = "copy-mode-key", key = "y" }
"C-g" = "clear-selection"
```

Built-in keys you don't rebind keep working. While you type a search, keys
are text.

## Rune shortcuts

The Rune desktop app has its own chords in `[rune.keys]`, separate from
`[keybindings]`. The value is a command, or `"none"` to take a built-in
chord away:

```toml
[rune]
key-defaults = true   # false: only your chords, plus palette and Settings until you bind them

[rune.keys]
"cmd+shift+d" = "split-right"
"ctrl+alt+left" = "previous-tab"
"cmd+d" = "none"
```

Modifiers are `cmd` (Super off macOS), `ctrl`, `alt`, and `shift`. Command
names are the kebab-case ids of the
[Rune command table](../clients/rune/src/keys/commands.rs) (`split-right`,
`next-tab`, `tab-1`, `palette`, `settings`, ...); an unknown name is reported
as `no command named NAME`. Menus still show the built-in chords.

## Inspecting and debugging

`phux config keys [ACTION]` prints every binding in effect, merged exactly as
the TUI merges them, with its source (`defaults`, a file name, or
`plugin <id>`). Bindings phux would skip are listed as warnings.

```text
$ phux config keys split-pane
prefix  C-a  (defaults)

KEY    ACTION      ARGS                  SOURCE
C-a "  split-pane  direction=horizontal  defaults
C-a %  split-pane  direction=vertical    defaults
```

`phux config check` reports the same problems with key paths:

| Finding | Fix |
| --- | --- |
| `unknown key name` | Use the syntax in [Writing keys](#writing-keys). |
| `unknown action` | Take the suggested name, or see [Actions](./reference/actions.md). |
| `ambiguous prefix` on a sequence | One key both runs an action and starts a sequence; remove one. |
| `ambiguous prefix` on a plain key | Two spellings of one key in a file; keep one. |
| `ambiguous prefix` in `global` | A global binding on the prefix key; move it. |

At attach a bad binding disables only itself; `phux config reload` refuses
the whole file and keeps your current bindings. A plugin key loses to yours
on the same key.
