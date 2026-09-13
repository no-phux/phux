---
audience: humans, contributors
stability: stable
last-reviewed: 2026-09-12
---

# Configuration and keybindings

**TL;DR.** phux loads `$XDG_CONFIG_HOME/phux/config.toml` as a sparse overlay
on the shipped defaults: omit a key and it keeps tracking those defaults.
Edit, `phux config check`, then `phux config reload` — the file is not
watched. `phux config show --layers` names which layer set each key.

---

## Config file location and discovery

phux loads configuration in this order:

1. **Shipped defaults** — embedded in the binary as `default.toml`
2. **Extended layers** — any files your config (or a layer) names via
   `extends`, in listed order
3. **User config** — `$XDG_CONFIG_HOME/phux/config.toml` (or
   `~/.config/phux/config.toml` if `$XDG_CONFIG_HOME` is not set)

Later files override earlier ones, key-by-key. A key you omit keeps the
default, so a phux upgrade reaches you automatically without losing your
overrides. phux does not expose a global config-path override; set
`XDG_CONFIG_HOME` when a command needs an isolated config tree.

The Unix socket is not a config key. Socket path, profile isolation, and
`PHUX_SOCKET` live in [`docs/reference/files.md`](./reference/files.md) and
[operations.md](./operations.md#instance-isolation-profiles).

### Getting started

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
```

### Applying changes

The edit loop is edit, check, reload:

```sh
$EDITOR ~/.config/phux/config.toml

phux config check        # every problem in one pass, with full dotted
                         # paths and the layer file that introduced each

phux config reload       # apply to running clients in place
```

`phux config reload` validates the layered config locally first — a broken
file fails right there with the parse error and signals nothing — then
rings a reload doorbell on the server so every attached client re-reads
its own config file and atomically rebuilds keybindings, the theme, the
status-bar composition, and plugin palette rows. On any parse or
validation error a client keeps its previous config fully in effect and
surfaces the error as a dismissable toast — never a half-applied mix. The
same reload is available inside the TUI as the `reload-config` action: a
command-palette row ("Reload the config file"), bindable to any chord
(unbound by default). See [`docs/consumers/tui.md`](./consumers/tui.md#reloading)
for the attach-side reload.

A few settings are read once at attach and still need a client restart
(detach and re-attach, or relaunch `phux`): `[experimental]` flags,
`[sidebar]` geometry, and `defaults.mouse`. `[defaults]` (except mouse),
`[voice]`, and `[[hooks.*]]` are owned by the server and take effect on
the next server start.

Reload is explicit, never automatic: the file is not watched, because
watch-reload introduces papercuts ("saved-mid-edit, now my keybindings
are gone"). An explicit verb keeps a broken intermediate save inert until
you ask for it.

Local config/plugin subcommands (`init`, `path`, `show`, `check`,
`plugins`, `agents`, `plugin ...`, and plugin action `run`) read the file
fresh on each invocation.

---

## Three concrete examples

### Example 1: Rebind the prefix from Ctrl-A to Ctrl-B

The shipped default is `C-a` to avoid conflicts with readline and screen.
To change it, edit `~/.config/phux/config.toml`:

```toml
[keybindings]
prefix = "C-b"
```

Then run `phux config reload` (or the `reload-config` palette action).
Every prefix-table binding (`c`, `%`, `x`, etc.) now fires after `Ctrl-B`
in every attached client, no restart needed.

Or use `Ctrl-Space`:

```toml
[keybindings]
prefix = "C-Space"
```

### Example 2: Switch the clock to a 12-hour format

The shipped right slot is session name and clock on a wide terminal, and
a `switch` chip below 65 columns. Changing the clock means assigning
`right`, which replaces that whole list — copy the shipped lineup and
edit the format, or you drop `switch`:

```toml
[status]
right = [
  { kind = "session-name", min-cols = 65 },
  { kind = "time", format = " %I:%M %p", min-cols = 65 },
  { kind = "switch", max-cols = 64 },
]
```

Run `phux config reload` to apply it. For styling (color, bold,
underline), use the universal `style` table in
[`docs/reference/widgets.md`](./reference/widgets.md).

### Example 3: Log a pane exit

```toml
[[hooks.pane-exit]]
when   = { exit-code = 0 }
action = "noop"

[[hooks.pane-exit]]
when   = { exit-code = "*" }
action = { kind = "run", command = "echo pane exited >> ~/.cache/phux/hooks.log" }
```

`phux config check` validates the surface; `phux config reload` is not
enough for hooks — the server reads them at start. The event table is
[`docs/reference/hooks.md`](./reference/hooks.md).

---

## Keybindings

The keybindings section has three keys:

- **`prefix`** — the key that unlocks prefix-table bindings (default:
  `C-a`)
- **`[keybindings.prefix-table]`** — bindings that fire after pressing
  the prefix. This is where `c` (new window), `%` (vertical split), `"`
  (horizontal split), `x` (kill pane), and the rest live.
- **`[keybindings.global]`** — bindings that fire any time, no prefix
  needed. Reserved for modifiers unlikely to conflict with inner
  programs: `super`, `hyper`, `meta`. Empty by default.

**Chord syntax:**

- `C-a` — Control+a
- `M-a` — Meta/Alt+a
- `S-a` or `A` — Shift+a
- `Tab`, `Enter`, `Esc` — named keys (case-sensitive)
- `F1` .. `F24` — function keys
- Punctuation with implicit Shift: `|`, `?`, `"` decompose to physical
  key + Shift on a US layout

**Resolution:** After pressing the prefix, the *next* keystroke is
matched against `prefix-table`. If it matches, the action runs; else the
keystroke goes to the pane. Global bindings are checked for every
keystroke; they fire if they match, else the keystroke goes to the pane.

A bare string is shorthand for a no-parameter action. Inline tables take
parameters. Your file overrides matching keys in the shipped defaults;
every other binding stays active:

```toml
[keybindings.prefix-table]
"x" = "kill-pane"
"|" = { action = "split-pane", direction = "vertical" }
"-" = { action = "split-pane", direction = "horizontal" }
"H" = { action = "resize-pane", direction = "left",  amount = 5 }
```

The action catalog is [`docs/reference/actions.md`](./reference/actions.md).

---

## Status bar

The status bar is rendered entirely client-side from three widget lists:
`left`, `center`, and `right`. A bare string like `"session-name"` is
shorthand for `{ kind = "session-name" }`. Widgets that take parameters
use inline table syntax.

**Assigning `right =` replaces the shipped right lineup.** The defaults
put session name and clock on a wide terminal and a `switch` chip below
65 columns. A `right = [...]` in your file drops all of that, including
`switch`. The same is true of `center` for `help-hints`. Use
`right-append` / `center-append` to add a widget; to change one widget,
copy the shipped list from `phux config show --default` and edit in
place.

The widget catalog is [`docs/reference/widgets.md`](./reference/widgets.md).
`phux config check` validates `[status]` through the same build path, so
a typo'd kind or option surfaces as a located finding.

---

## Scrollback

Per-pane history has a line bound (`defaults.history-limit`) and a byte
bound (`defaults.history-bytes`); libghostty prunes on whichever is
reached first. On anything but a narrow grid the byte bound is what
binds, so **raising `history-limit` on a wide grid buys no extra
scrollback.** Raise `history-bytes` if you want depth. That is attach
latency, not just memory: on attach the server re-encodes every retained
page of every pane in the session, on one thread. The measured costs and
the 64 MiB cap live in the comments of the shipped defaults (`phux config
show --default`; also the annotated file in
[`docs/reference/config.md`](./reference/config.md)).

---

## Hooks

Hooks are event-driven actions the server fires. A starter set ships
today — `after-new-pane`, `pane-exit`, `focus-changed`,
`client-attached`, `client-detached`, and `agent-state-changed` — and the
shipped defaults define none. Each `[[hooks.<name>]]` entry is an
array-of-tables row; multiple entries are allowed and the first match
wins per event.

```toml
[[hooks.pane-exit]]
when   = { exit-code = "*" }
action = { kind = "run", command = "echo pane exited >> ~/.cache/phux/hooks.log" }
```

`phux config check` validates event names, `when` keys, and actions; the
server warns again at startup about a hook that can never fire. The event
table, context keys, and `PHUX_*` environment are
[`docs/reference/hooks.md`](./reference/hooks.md).

---

## Plugins

Plugins are executable packages declared by a `phux-plugin.toml`. Link
one, then list or toggle it:

```sh
phux plugin link ./my-plugin/phux-plugin.toml
phux plugin list
phux plugin enable example.agent-tools
phux plugin disable example.agent-tools
```

Enabled actions appear in the attach command palette. An action may
declare a prefix-table `keys` chord; user `[keybindings]` always win on
conflict. There is no in-process plugin host: commands run as argv from
the plugin root.

---

## Layered configs: `extends`

A config file may name shared layers — a team baseline, a curated
distribution — with a top-level `extends` key
([ADR-0039](adr/0039-layered-config.md)):

```toml
extends = ["distro.toml", "minimal"]

[keybindings]
prefix = "C-b"        # your overrides win over every layer
```

Rules:

- **Order.** Layers merge in listed order, each atop the previous; your
  file merges last and wins per key. The shipped defaults always sit at
  the bottom.
- **Resolution.** An entry with a path separator or a `.toml` suffix is a
  path, resolved relative to the directory of the file that declares it
  (absolute paths pass through). A bare name `n` means `layers/n.toml`
  beside the declaring file — so `extends = ["minimal"]` in
  `~/.config/phux/config.toml` loads
  `~/.config/phux/layers/minimal.toml`.
- **Layers can extend layers**, up to 4 levels below your file. Cycles,
  missing layer files, and over-deep nesting are errors that name the
  offending file. A layer reachable through two branches merges once.

### Array merge: replace by default, `-append` to add

Tables merge per key across layers, but an array assignment replaces the
inherited array wholesale — TOML arrays have no per-element identity to
merge on. When a layer should *contribute to* a list instead of owning
it, use the `-append` key suffix:

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

`x-append` must hold an array and appends its elements to the stack's
current `x` (creating it when absent). Setting both `x` and `x-append` in
one file, appending to a non-array, or a non-array `-append` value are
errors naming that file. Keybindings need no append form: `prefix-table`
and `global` are tables and already merge per chord. The `-append` suffix
is reserved at every level; don't end a free-form key (for example a
`[theme]` slot) with it. To *drop* an inherited entry, assign the full
array plainly — replacement always wins over inheritance.

**Plugin manifests in layers.** A relative `manifest` in `[[plugins]]` /
`[[plugins-append]]` normally resolves against *your config file's*
directory. Inside an extended layer that base would be wrong — the layer
lives elsewhere — so layer resolution rewrites a relative manifest to an
absolute path under the layer file's own directory (lexically normalized)
before merging. Your root `config.toml` is left verbatim; only extended
layers are rewritten. This is what lets a distro wire plugins that live
next to it.

### Where did this value come from?

With several layers in play, `phux config show` tells you *what* the
effective config is but not *who* set it. `phux config show --layers`
answers that: it prints the resolved layer stack in merge order, then one
row per effective leaf key naming the layer that set it. Arrays expand to
one row per element, so an `-append` list shows exactly which layer
contributed each entry:

```
layers (merge order; later layers win):
  [1] defaults (embedded)
  [2] /home/me/.config/phux/distro.toml
  [3] /home/me/.config/phux/config.toml (user)

keys:
  defaults.history-bytes  <- [1] defaults
  defaults.history-limit  <- [2] distro.toml
  keybindings.prefix      <- [3] user
  status.right[0]         <- [1] defaults
  status.right[1]         <- [1] defaults
  status.right[2]         <- [2] distro.toml
```

`--layers --json` emits the same information as a stable document
(`schema_version` 1): a `layers` array (1-based `index`, `kind` of
`defaults` / `extended` / `user`, `path`) and a `keys` array (`key`,
owning `layer` index, and for arrays an `element_layers` list, one entry
per element).

### Starter distributions: `config init --distro`

A *distro* is a config layer curated as a starting point — the lazyvim
idea applied to phux: keybindings, a status lineup, a theme, and a plugin
set, shipped as one referenced file rather than pasted into yours. The
repo bundles one, [`starter`](../distros/starter/README.md), which today
carries only the demo plugin set: the keybindings, status lineup, and
theme it used to add are now the shipped defaults, because a setting
everyone should have does not belong behind an opt-in. A distro is for
offering a genuine choice.

```sh
phux config init --distro starter            # bundled name
phux config init --distro ./my/layer.toml  # or any path (a directory
                                           #   means <dir>/<dirname>.toml)
```

This writes the usual commented starter config with exactly one live
statement at the top:

```toml
extends = ["/absolute/path/to/distros/starter/starter.toml"]
```

Nothing is copied out of the distro. Your file stays a sparse overlay:
keys you set win over the distro, keys the distro sets win over the
shipped defaults, and updating the distro file updates every config that
extends it. `init --distro` validates the full merged stack before
writing anything, so a broken or missing distro layer fails the command
instead of leaving you an invalid config; `phux config show` then renders
the effective result.

A bundled name `n` resolves to `<dir>/n/n.toml` across, in order:
`$PHUX_DISTROS_DIR` (explicit override), `$XDG_DATA_HOME/phux/distros`
(default `~/.local/share/phux/distros`), and — as a dev-build convenience
— the repo checkout's `distros/` directory. An unknown name lists every
path that was checked. `--distro herdr` still resolves as an alias of
`starter`. Configs that already `extends` `distros/herdr/herdr.toml`
keep loading: the path remains as a stub, and the loader rewrites a
missing herdr file to `distros/starter/starter.toml` when that file
exists.

---

## Other knobs

**Theme.** `[theme]` is a free-form map of named slots. Set one; the rest
keep the shipped palette:

```toml
[theme]
accent = "#7aa2f7"
```

Slot names and the shipped colors are in
[`docs/reference/config.md`](./reference/config.md).

**Sidebar.** On by default. `enabled`, `width` (`0` adapts to 28–40
columns; a positive width is fixed), and `position` (`left` or `right`)
are read at attach — `phux config reload` does not apply `[sidebar]`.
Detach and re-attach. `prefix-b` toggles it for the life of that attach.

**Federation and remotes.** `[[satellites]]`, `[[remote]]`, and
`[[connector]]` are in the generated schema. Tokens stay in owner-only
files, never inline. Enroll a host with the commands in
[Remote access](./remote-access.md); do not hand-edit a token into
`config.toml`.

---

## Links

**Generated (cannot drift):**

- **Full schema and annotated defaults** → [`docs/reference/config.md`](./reference/config.md)
- **Action catalog** → [`docs/reference/actions.md`](./reference/actions.md)
- **Widget catalog** → [`docs/reference/widgets.md`](./reference/widgets.md)
- **Hook events** → [`docs/reference/hooks.md`](./reference/hooks.md)
- **File locations** → [`docs/reference/files.md`](./reference/files.md)
- **CLI inventory** → [`docs/reference/cli.md`](./reference/cli.md)

**Narrative:**

- **Attach TUI** → [`docs/consumers/tui.md`](./consumers/tui.md)
- **Getting started** → [`docs/QUICKSTART.md`](./QUICKSTART.md)
- **Shipped defaults with comments** → `phux config show --default`
