---
audience: humans, contributors
stability: stable
last-reviewed: 2026-08-02
---

# Configuration and keybindings

**TL;DR.** Config lives in `$XDG_CONFIG_HOME/phux/config.toml` (or `~/.config/phux/config.toml`); phux merges your file atop the shipped defaults, with an optional `extends` stack of shared layers in between. Customize keybindings (prefix + global chords), status bar widgets, and hooks via TOML tables.

---

## Config file location and discovery

phux loads configuration in this order:

1. **Shipped defaults** — embedded in the binary as `default.toml`
2. **Extended layers** — any files your config (or a layer) names via `extends`, in listed order (see the next section)
3. **User config** — `$XDG_CONFIG_HOME/phux/config.toml` (or `~/.config/phux/config.toml` if `$XDG_CONFIG_HOME` is not set)

Later files override earlier ones, key-by-key. A key you omit keeps the default, so a phux upgrade reaches you automatically without losing your overrides. phux does not currently expose a global config-path override; set `XDG_CONFIG_HOME` when a command needs an isolated config tree.

See "Layered configs" below for the `extends` mechanics.

### Getting started

To scaffold a documented starter config:

```sh
phux config init         # creates ~/.config/phux/config.toml
                         # (refuses to overwrite; use --force to override)

phux config path         # print the resolved config path (no I/O)

phux config show         # print the effective config (defaults merged
                         # with your overrides) as canonical TOML

phux config show --default  # print the shipped defaults with comments

phux config show --layers   # provenance: which layer (defaults, an
                            # extends layer, or your file) set each
                            # effective key; --json for the stable
                            # machine-readable form

phux config check        # validate: every unknown key and wrong value,
                         # each with its full dotted path and the layer
                         # file that introduced it

phux config reload       # apply edits to running clients in place
                         # (see "Applying changes" below)

phux plugin link ./my-plugin/phux-plugin.toml --json
                         # add or update a plugin manifest entry

phux plugin list --json  # inspect the plugin registry

phux plugin disable example.agent-tools --json
phux plugin enable example.agent-tools --json
                         # toggle a registered plugin

phux plugin unlink example.agent-tools --json
                         # remove a registered plugin

phux config plugins --json  # legacy read path for configured manifests
phux config agents --json   # project configured plugin agent states
phux config run PLUGIN ACTION --json  # execute a configured plugin action
```

### Applying changes

The edit loop is edit, check, reload:

```sh
$EDITOR ~/.config/phux/config.toml

phux config check        # every problem in one pass, with full dotted
                         # paths and the layer file that introduced each

phux config reload       # apply to running clients in place
```

`phux config reload` validates the layered config locally first — a broken file fails right there with the parse error and signals nothing — then rings a reload doorbell on the server so every attached client re-reads its own config file and atomically rebuilds keybindings, the theme, the status-bar composition, and plugin palette rows. On any parse or validation error a client keeps its previous config fully in effect and surfaces the error as a dismissable toast — never a half-applied mix. The same reload is available inside the TUI as the `reload-config` action: a command-palette row ("Reload the config file"), bindable to any chord (unbound by default). See `docs/consumers/tui.md` §4.3 for the full reload semantics.

A few settings are read once at attach and still need a client restart (detach and re-attach, or relaunch `phux`): `[experimental]` flags, `[sidebar]` geometry, and `[defaults]` (which the server owns anyway).

Reload is explicit, never automatic: the file is not watched, because watch-reload introduces papercuts ("saved-mid-edit, now my keybindings are gone"). An explicit verb keeps a broken intermediate save inert until you ask for it.

Local config/plugin subcommands (`init`, `path`, `show`, `check`, `plugins`, `agents`, `plugin ...`, and plugin action `run`) read the file fresh on each invocation.

---

## Layered configs: `extends`

A config file may name shared layers — a team baseline, a curated distribution — with a top-level `extends` key ([ADR-0039](../ADR/0039-layered-config.md)):

```toml
extends = ["distro.toml", "minimal"]

[keybindings]
prefix = "C-b"        # your overrides win over every layer
```

Rules:

- **Order.** Layers merge in listed order, each atop the previous; your file merges last and wins per key. The shipped defaults always sit at the bottom.
- **Resolution.** An entry with a path separator or a `.toml` suffix is a path, resolved relative to the directory of the file that declares it (absolute paths pass through). A bare name `n` means `layers/n.toml` beside the declaring file — so `extends = ["minimal"]` in `~/.config/phux/config.toml` loads `~/.config/phux/layers/minimal.toml`.
- **Layers can extend layers**, up to 4 levels below your file. Cycles, missing layer files, and over-deep nesting are errors that name the offending file. A layer reachable through two branches merges once.

### Array merge: replace by default, `-append` to add

Tables merge per key across layers, but an array assignment replaces the inherited array wholesale — TOML arrays have no per-element identity to merge on. When a layer should *contribute to* a list instead of owning it, use the `-append` key suffix:

```toml
# In a distro layer or your own config:

[[plugins-append]]                      # adds to inherited [[plugins]]
manifest = "/opt/distro/phux-plugin.toml"

[status]
right-append = [{ kind = "time", format = "%H:%M" }]   # adds a widget

[[hooks.pane-exit-append]]              # adds a pane-exit hook
when   = { exit-code = "*" }
action = "noop"
```

`x-append` must hold an array and appends its elements to the stack's current `x` (creating it when absent). Setting both `x` and `x-append` in one file, appending to a non-array, or a non-array `-append` value are errors naming that file. Keybindings need no append form: `prefix-table` and `global` are tables and already merge per chord. The `-append` suffix is reserved at every level; don't end a free-form key (for example a `[theme]` slot) with it. To *drop* an inherited entry, assign the full array plainly — replacement always wins over inheritance.

**Plugin manifests in layers.** A relative `manifest` in `[[plugins]]` / `[[plugins-append]]` normally resolves against *your config file's* directory. Inside an extended layer that base would be wrong — the layer lives elsewhere — so layer resolution rewrites a relative manifest to an absolute path under the layer file's own directory (lexically normalized) before merging. Your root `config.toml` is left verbatim; only extended layers are rewritten. This is what lets a distro wire plugins that live next to it.

### Where did this value come from?

With several layers in play, `phux config show` tells you *what* the effective config is but not *who* set it. `phux config show --layers` answers that: it prints the resolved layer stack in merge order, then one row per effective leaf key naming the layer that set it. Arrays expand to one row per element, so an `-append` list shows exactly which layer contributed each entry:

```
layers (merge order; later layers win):
  [1] defaults (embedded)
  [2] /home/me/.config/phux/distro.toml
  [3] /home/me/.config/phux/config.toml (user)

keys:
  defaults.history-limit  <- [2] distro.toml
  keybindings.prefix      <- [3] user
  status.right[0]         <- [1] defaults
  status.right[1]         <- [1] defaults
  status.right[2]         <- [2] distro.toml
```

`--layers --json` emits the same information as a stable document (`schema_version` 1): a `layers` array (1-based `index`, `kind` of `defaults` / `extended` / `user`, `path`) and a `keys` array (`key`, owning `layer` index, and for arrays an `element_layers` list, one entry per element).

### Starter distributions: `config init --distro`

A *distro* is a config layer curated as a starting point — the lazyvim idea applied to phux: keybindings, a status lineup, a theme, and a plugin set, shipped as one referenced file rather than pasted into yours. The repo bundles one, [`herdr`](../distros/herdr/README.md).

```sh
phux config init --distro herdr            # bundled name
phux config init --distro ./my/layer.toml  # or any path (a directory
                                           #   means <dir>/<dirname>.toml)
```

This writes the usual commented starter config with exactly one live statement at the top:

```toml
extends = ["/absolute/path/to/distros/herdr/herdr.toml"]
```

Nothing is copied out of the distro. Your file stays a sparse overlay: keys you set win over the distro, keys the distro sets win over the shipped defaults, and updating the distro file updates every config that extends it. `init --distro` validates the full merged stack before writing anything, so a broken or missing distro layer fails the command instead of leaving you an invalid config; `phux config show` then renders the effective result.

A bundled name `n` resolves to `<dir>/n/n.toml` across, in order: `$PHUX_DISTROS_DIR` (explicit override), `$XDG_DATA_HOME/phux/distros` (default `~/.local/share/phux/distros`), and — as a dev-build convenience — the repo checkout's `distros/` directory. An unknown name lists every path that was checked.

---

## Schema overview

The complete schema is a generated reference: [`docs/reference/config.md`](./reference/config.md) lists every section, every scalar knob with its shipped default, and the annotated default config embedded in the binary. It renders from the compiled binary and is byte-pinned by a freshness test, so it cannot drift from the code. The subsections below cover only the material that benefits from narrative — keybindings, widgets, hooks, and plugins.

### Keybindings

The keybindings section has three keys:

- **`prefix`** — the key that unlocks prefix-table bindings (default: `C-a`)
- **`[keybindings.prefix-table]`** — bindings that fire after pressing the prefix (tmux-style). This is where `c` (new window), `%` (vertical split), `"` (horizontal split), `x` (kill pane), etc. live.
- **`[keybindings.global]`** — bindings that fire any time, no prefix needed. Reserved for modifiers unlikely to conflict with inner programs: `super`, `hyper`, `meta`. Empty by default.

**Chord syntax:**
- `C-a` — Control+a
- `M-a` — Meta/Alt+a  
- `S-a` or `A` — Shift+a
- `Tab`, `Enter`, `Esc` — named keys (case-sensitive)
- `F1` .. `F24` — function keys
- Punctuation with implicit Shift: `|`, `?`, `"` decompose to physical key + Shift on a US layout

**Resolution:** After pressing the prefix, the *next* keystroke is matched against `prefix-table`. If it matches, the action runs; else the keystroke goes to the pane. Global bindings are checked for every keystroke; they fire if they match, else the keystroke goes to the pane.

**Actions** are typed commands with optional parameters. The full action catalog is a generated reference: [`docs/reference/actions.md`](./reference/actions.md). A bare string is shorthand for a no-parameter action:

```toml
[keybindings.prefix-table]
"x"        = "kill-pane"              # bare string
"c"        = { action = "new-window" } # same thing, inline table form
"|"        = { action = "split-pane", direction = "vertical" }
```

### Status bar widgets

The status bar is rendered entirely client-side from a list of widgets:

```toml
[status]
left   = [{ kind = "windows" }]
center = []
right  = ["session-name", { kind = "time", format = "%H:%M" }]
```

A bare string like `"session-name"` is shorthand for `{ kind = "session-name" }`. Widgets that take parameters use inline table syntax.

The widget catalog is a generated reference: [`docs/reference/widgets.md`](./reference/widgets.md) lists every registered kind — `session-name`, `windows`, `time`, `cwd`, `exit`, `exec`, `help-hints` — with the exact options and defaults each factory accepts, plus the universal `style` table for colors, attributes, and separators. A kind or option is listed there exactly when the binary accepts it. `phux config check` validates `[status]` through the same build path, so a typo'd kind or option surfaces as a located finding.

All are **optional**. The default ships with windows on the left and session name + time on the right. See `phux config show --default` for styled examples with custom colors and separators.

### Hooks (events and actions)

Hooks are event-driven actions, fired by the server-side dispatcher (`phux-server::hooks`). A starter set of real events ships today — `after-new-pane`, `pane-exit`, `focus-changed`, `client-attached`, `client-detached`, and `agent-state-changed` — and the shipped defaults define no hooks. You could, for example, declare two `pane-exit` hooks — one for success (exit code 0), one that logs any other exit:

```toml
[[hooks.pane-exit]]
when   = { exit-code = 0 }
action = "noop"

[[hooks.pane-exit]]
when   = { exit-code = "*" }
action = { kind = "run", command = "echo pane exited >> ~/.cache/phux/hooks.log" }
```

Each `[[hooks.<name>]]` entry is an array-of-tables entry; multiple entries are allowed and the first match wins per event. `phux config check` validates the whole surface — event names, `when` keys, and actions — and the server warns again at startup about a hook that can never fire. See `docs/consumers/tui.md` §9 for the full event table, context keys, and execution semantics.

### Plugins

> **Status:** `[[plugins]]` entries parse, `phux plugin` manages their
> lifecycle, `phux plugin list --json` / `phux config plugins --json` list
> manifests, `phux config agents --json` projects declared agent state for
> consumers, and `phux config run PLUGIN ACTION` executes action entries. Event
> hooks, panes, links, and workspace profiles are declarative provider records;
> plugin actions compose the shipped CLI surfaces such as `workspace save` and
> `workspace restore`.

Plugins are executable workflow packages declared by a `phux-plugin.toml`
manifest. The config file composes local manifests:

```toml
[[plugins]]
manifest = "/path/to/plugin/phux-plugin.toml"
enabled = true
```

`manifest` may be absolute or relative to `config.toml`. A minimal manifest:

```toml
id = "example.agent-tools"
name = "Agent Tools"
version = "0.1.0"
min_phux_version = "0.0.2"

[[actions]]
id = "summarize"
title = "Summarize pane"
contexts = ["pane"]
command = ["python3", "summarize.py"]
keys = "g"   # optional TUI prefix-table binding; user config wins on conflict

[[agents]]
id = "codex"
label = "Codex"
description = "Coding agent"
state = "blocked"      # unknown | idle | working | blocked
attention = "high"     # none | low | normal | high
contexts = ["workspace", "pane"]
```

The `phux plugin` verbs edit the same `[[plugins]]` array while preserving
relative manifest paths already in the file:

```sh
phux plugin link ./plugins/agent-tools/phux-plugin.toml
phux plugin validate
phux plugin disable example.agent-tools
phux plugin enable example.agent-tools
phux plugin unlink example.agent-tools
```

Run the action:

```sh
phux config run example.agent-tools summarize --json
```

Action commands execute as argv arrays from the plugin root. phux captures
stdout/stderr, exit status, duration, and timeout outcome into a
`schema_version = 1` JSON result when `--json` is set. There is no hidden shell
expansion; a manifest only gets shell behavior when it explicitly declares an
argv such as `["sh", "-c", "…"]`. The runtime inherits the phux process
environment and adds `PHUX_PLUGIN_ID`, `PHUX_PLUGIN_ACTION_ID`, and
`PHUX_PLUGIN_ROOT`.

Enabled plugins' actions also surface in the attach TUI: every action gets
a command-palette row, and an action may declare an optional
`keys = "..."` chord that merges into the TUI's prefix table (user
`[keybindings]` always win on conflict). See
[`consumers/tui.md`](./consumers/tui.md) §5.5.

The manifest format also accepts `[[build]]`, `[[events]]`, `[[panes]]`,
`[[links]]`, and `[[workspaces]]` entries. Agent declarations are static status
records for consumer projections: `state` normalizes to `unknown`, `idle`,
`working`, or `blocked`, and `attention` normalizes to `none`, `low`, `normal`,
or `high`. Event hooks, panes, link handlers, and workspace profiles are
provider-shaped: each entry has a plugin-local `id`, a `title`, and, where it
executes, an argv `command`, so frontends and server layers can enumerate them
without loading plugin code. Link handlers additionally declare `schemes` or
`patterns`; workspace profiles list the action, event, agent, and pane role ids
they compose. Commands are argv arrays, not shell strings. This keeps phux core
small: the plugin owns its language and files, while phux owns manifest
validation, config composition, workspace archives, and terminal control.

[`examples/plugins/provider-showcase/phux-plugin.toml`](../examples/plugins/provider-showcase/phux-plugin.toml)
is the checked-in provider fixture for event, pane, and link-handler
enumeration.

The checked-in demo package at
[`examples/plugins/agent-tools`](../examples/plugins/agent-tools/README.md)
shows the smallest useful loop: point `XDG_CONFIG_HOME` at the fixture config,
list the configured plugin, validate it with `--json`, then run its `inspect`
action. From a fresh checkout:

```sh
just plugin-demo
```

The same package carries first-party public Codex and Claude Code integration
records. Link the plugin itself with `phux plugin link` or the fixture config,
then use plugin actions for package lifecycle checks:

```sh
phux config run com.phux.demo.agent-tools validate-integrations
phux config run com.phux.demo.agent-tools link-integration
phux config run com.phux.demo.agent-tools status-integrations
phux config run com.phux.demo.agent-tools unlink-integration
```

Those actions are still external and declarative. They write plugin-local
state files, report package state as `missing`, `current`, or `outdated`, and
record either a native session id supplied by the caller or the phux session
target. They do not load an in-process plugin host, contact private services,
or require agent credentials. `smoke-integrations` runs the lifecycle against
fake public CLIs in a temporary state directory.

Two larger checked-in profiles exercise the workspace layer:

- [`examples/plugins/continuum`](../examples/plugins/continuum/phux-plugin.toml)
  declares `autosave` and `restore-latest` actions plus idle/session-change
  events that call `phux workspace save` and `phux workspace restore`.
- [`examples/plugins/agent-tools`](../examples/plugins/agent-tools/README.md)
  declares an `agent-bench` workspace whose `launch-bench`, `list-bench`, and
  `drive-bench` actions create role sessions, report status, and route keys to
  the selected role through `phux send-keys`.

---

## Three concrete examples

### Example 1: Rebind the prefix from Ctrl-A to Ctrl-B

The shipped default is `C-a` to avoid conflicts with readline and screen. To change it, edit `~/.config/phux/config.toml`:

```toml
[keybindings]
prefix = "C-b"
```

Then run `phux config reload` (or the `reload-config` palette action). Every prefix-table binding (`c`, `%`, `x`, etc.) now fires after `Ctrl-B` in every attached client, no restart needed.

Or use `Ctrl-Space`:

```toml
[keybindings]
prefix = "C-Space"
```

### Example 2: Switch the clock to a 12-hour format

Suppose you want the right status bar to show the session name and a 12-hour clock. Edit your config:

```toml
[status]
right = [
  "session-name",
  { kind = "time", format = " %I:%M %p" }
]
```

Run `phux config reload` to apply it. The status bar now shows the session name and a 12-hour time on the right. For styling (color, bold, underline), use the universal `style` table documented in [`docs/reference/widgets.md`](./reference/widgets.md).

### Example 3: Customize the prefix-table to use Vim-style bindings

The shipped defaults use `h/j/k/l` for directional focus (Vim-style) and `c` for new window. Suppose you want to remap to HJKL (uppercase) for resize, and add a binding for splitting horizontally with `-`:

```toml
[keybindings]
prefix = "C-a"

[keybindings.prefix-table]
# Include the shipped defaults (or override as needed).
# This example shows the resize bindings:
"H" = { action = "resize-pane", direction = "left",  amount = 5 }
"J" = { action = "resize-pane", direction = "down",  amount = 5 }
"K" = { action = "resize-pane", direction = "up",    amount = 5 }
"L" = { action = "resize-pane", direction = "right", amount = 5 }
"-" = { action = "split-pane", direction = "horizontal" }
```

Your file overrides the matching keys in the shipped defaults; all other bindings remain active. Run `phux config reload` to apply it. Now `Ctrl-A H` resizes the pane left by 5 columns, and `Ctrl-A -` splits horizontally.

---

## Links and next steps

**Dive deeper (generated, cannot drift):**
- **Full schema and annotated defaults** → [`docs/reference/config.md`](./reference/config.md)
- **Action catalog** → [`docs/reference/actions.md`](./reference/actions.md)
- **Widget catalog** → [`docs/reference/widgets.md`](./reference/widgets.md)
- **CLI inventory** → [`docs/reference/cli.md`](./reference/cli.md)

**Narrative:**
- **Hook events and execution semantics** → [`docs/consumers/tui.md`](./consumers/tui.md) §9
- **Getting started** → [`docs/QUICKSTART.md`](./QUICKSTART.md)
- **Understanding the TUI model** → [`docs/consumers/tui.md`](./consumers/tui.md) §2–3
- **Shipped defaults with comments** → `phux config show --default`
