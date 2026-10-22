# Configuration file

phux reads one TOML file, `config.toml`, for your key bindings, theme,
status bar, sidebar, hooks, plugins, and the defaults new panes start
with. It's a sparse overlay: any key you set replaces the shipped value,
and everything you leave out keeps the default.

You don't need one. phux ships with a full set of defaults, so start
without a file and write one when there is something you want to change.

## File location

The file is `$XDG_CONFIG_HOME/phux/config.toml` when that variable is set,
and `~/.config/phux/config.toml` when it isn't. `phux config path` prints
the path phux resolved, whether or not the file exists:

```sh
$ phux config path
/Users/you/.config/phux/config.toml
```

There is no flag or variable that points phux at a different file. To try
a configuration in isolation, set `XDG_CONFIG_HOME` to another directory
for that shell.

The server and every client read the file on the machine they run on. A
client attached to a remote server uses its own machine's key bindings and
theme; the server's `[defaults]` and hooks come from the server's machine.

## Quick example

This file changes the prefix to <kbd>C-b</kbd>, binds <kbd>C-b</kbd>
<kbd>r</kbd> to reload the configuration, and moves the status bar to the
bottom:

```toml
[keybindings]
prefix = "C-b"

[keybindings.prefix-table]
"r" = "reload-config"

[status]
position = "bottom"
```

Apply it to every attached client:

```sh
$ phux config reload
config OK; reload signalled to attached clients and a running hub
```

Press <kbd>C-b</kbd> <kbd>r</kbd> after your next edit and the same reload
runs from inside phux. The [config reference](./reference/config.md) lists
every section and setting with its default.

## Applying changes

phux never watches the file. Changes apply when you ask, and different
settings are read at different moments.

`phux config reload` validates the file first. If anything is wrong it
signals nothing and tells you why:

```text
$ phux config reload
phux: error: config invalid, not signalling reload: keybinding "q": unknown action `kill-pain` (did you mean `kill-pane`?)
  hint: run `phux config check` to list every problem
```

A client whose own re-read fails keeps its previous settings and shows the
error as a toast. A reload is never half-applied.

### When a change takes effect

| Settings | Take effect |
| --- | --- |
| Key bindings, which-key, theme, status bar, `[chrome]`, `[notifications]`, `clipboard-write`, plugin actions, panes and sidebar sections | On `phux config reload` (or the `reload-config` action). |
| `[sidebar]`, `mouse`, `[experimental] predictive-echo` | The next time you attach. Detach and run `phux attach` again. |
| The rest of `[defaults]` (shell, `TERM`, scrollback, session names, ...), `[[hooks]]`, plugin event hooks, `[policy]`, `[voice]`, `[limits]` | When the server starts. |
| `[[satellites]]`, `[[uplink]]` | On `phux config reload`, by the server. |

The server reads its settings once, at start. `phux upgrade` restarts the
server in place without closing any pane, so it is the way to apply them
to a running server. The server refuses to start on a file that does not
load, so run `phux config check` first.

The Settings page shows each setting's own timing in its detail panel.

## Validation

`phux config check` reads the file without applying it and reports every
problem in one pass, each with its full key path:

```sh
$ phux config check
/Users/you/.config/phux/config.toml: ok
```

With no file it says so and exits 0, since the defaults apply. Give it a
path to check a draft before you move it into place:
`phux config check ~/dotfiles/phux.toml`.

| Exit code | Meaning |
| --- | --- |
| 0 | No problems. |
| 1 | It found problems, listed one per line. |
| 2 | The check could not run: a file you named is missing or unreadable, or the TOML does not parse. |

`--json` prints the same findings as a stable JSON document, so the check
can gate a dotfiles CI run.

### Findings

Each line is the key path, the kind of problem, and the detail:

```text
$ phux config check
sidebar.enabledd: unknown key: unknown field `enabledd`, expected one of `enabled`, `width`, `position`, `agents-share`, `hosts`, `hosts-provider`, `hosts-refresh-secs`
keybindings.prefix-table.q: unknown name: unknown action `kill-pain` (did you mean `kill-pane`?)
status.right[0]: bad value: invalid option for widget time: unknown option `formt` (did you mean `format`?)
3 problems
```

| Kind | What it means |
| --- | --- |
| `unknown key` | The section has no such setting, usually a typo. |
| `bad value` | The setting exists, but the value has the wrong type, is out of range, or a widget option is wrong. |
| `bad chord` | A key that does not parse, or two bindings that collide. See [Key bindings](./keys.md#debugging). |
| `unknown name` | An action, hook event, `when` key, or widget kind phux doesn't know. |
| `dead action` | A hook whose action can never run. See [Hooks](./hooks.md#debugging). |

When a problem comes from a file you extend rather than your own, the line
after it names that file (`  from /path/to/layer.toml`).

**Check the file before you attach.** A file that does not load at all,
because of a TOML syntax error or an unknown key, still lets you attach:
phux ignores the whole file, runs on the shipped defaults (so `C-a d` still
detaches), and shows the error on the status bar until you fix it and run
`phux config reload`. A reload of a file that still fails keeps whatever
config you had. A server won't start on such a file. A file that loads but has one bad key binding loses only that
binding; the rest still work.

## Starter file

`phux config init` writes a commented copy of every shipped setting to
your config path. Every line is commented out, so the file changes nothing
until you uncomment a line:

```sh
$ phux config init
wrote /Users/you/.config/phux/config.toml
```

It refuses to overwrite an existing file unless you pass `--force`.
`phux config show --default` prints the same defaults without writing
anything, and `phux config show` prints the merged result in effect.

`phux config init --distro starter` writes the same file plus an `extends`
line that layers the starter distribution under it. The starter turns on
the example plugins (workspace autosave, agent helpers, plan usage). It is
found in a phux source checkout, or in the directory `PHUX_DISTROS_DIR`
names.

## Layers

A file can build on others with `extends`. Your file applies last, so it
wins every key it sets:

```toml
extends = ["work.toml"]

[keybindings]
prefix = "C-b"
```

The layers apply in this order: the shipped defaults, each extended file
in the order listed, then the file that extends them. An extended file can
extend others in turn.

- An absolute path is used as written.
- A relative path, or any name ending in `.toml`, is relative to the file
  that names it.
- A bare name like `work` means `layers/work.toml` beside that file.

Tables merge key by key. An array replaces the one beneath it, unless you
spell its key with `-append`, which adds to the inherited array instead:

```toml
[status]
right-append = [{ kind = "cwd" }]
```

That keeps the shipped right side of the status bar and adds the working
directory after it. You can't set `right` and `right-append` in the same
table.

`phux config show --layers` shows which layer set each key in effect:

```text
$ phux config show --layers
layers (merge order; later layers win):
  [1] defaults (embedded)
  [2] /Users/you/.config/phux/work.toml
  [3] /Users/you/.config/phux/config.toml (user)

keys:
  defaults.agent-log-bytes                 <- [1] defaults
  ...
  keybindings.prefix                       <- [3] user
  ...
  status.position                          <- [2] work.toml
  ...
```

## Settings page

Press <kbd>C-a</kbd> <kbd>S</kbd> to open the Settings page. It lists
every setting with its value in effect, its default, the layer that set
it, and when a change applies. Edits go straight into your
`config.toml` and then reload, like `reload-config`.

- Type to filter across sections. <kbd>Tab</kbd> steps between sections.
- <kbd>Enter</kbd> or <kbd>Space</kbd> toggles, cycles, or edits the row;
  <kbd>Left</kbd> and <kbd>Right</kbd> step a value.
- <kbd>Delete</kbd> or <kbd>C-r</kbd> removes your override so the default
  shows through. <kbd>C-z</kbd> undoes.

Each edit changes one line and keeps your comments. An edit that would
make the file invalid is refused before it is written. A setting that
comes from a file you extend can't be reset here; the page names that
file so you can edit it.

## Predictive echo

Predictive echo shows your typing in a pane before the server echoes it
back, which hides latency on a slow link. It is on when you attach to
another machine and off for a server on this one. Set it explicitly to
override that:

```toml
[experimental]
predictive-echo = false
```

**Turn it off before you type secrets on a shared screen.** Prediction
does not know when a program has turned echo off, so characters you type
at a password prompt can appear briefly on your screen until the server's
reply corrects them, or for up to about five seconds. It only changes what
your own client draws: nothing extra is sent, and other clients and
recordings never see it.

The setting is read when you attach. After changing it, detach and run
`phux attach` again; neither `phux config reload` nor an automatic
reconnect turns off prediction in a session that already has it.

## Debugging

- `phux config check` lists every problem in the file, with key paths.
- `phux config show` prints the merged settings in effect;
  `--layers` names the file that set each key, and `--default` prints the
  shipped defaults.
- `phux config keys` lists every key binding and where it came from.
- `phux config path` prints the file phux reads.

If a setting seems to have no effect, check the table under
[When a change takes effect](#when-a-change-takes-effect): a `[sidebar]`
change needs a new attach, and a `[defaults]` change needs the server to
restart.

## More

- [Key bindings](./keys.md): change the prefix, rebind and unbind keys.
- [Themes](./themes.md): install and pick colour themes, or recolour one
  part of the chrome.
- [Status bar and sidebar](./status-bar.md): what the bar and sidebar
  show, and agent notifications.
- [Hooks](./hooks.md): run a command when something happens.
- [Plugins](./plugins.md): add actions, panes, widgets, and themes.
- [Projects](./projects.md): open a repository's session from a recipe.
- [Config reference](./reference/config.md): every section and setting.
