# Plugins

A plugin is a directory with a `phux-plugin.toml` manifest that adds to
phux: actions for the palette and your keys, panes, status-bar widgets,
sidebar sections, themes, hooks, and agent declarations. Install one to
pick up someone else's workflow, or write one to package your own.

```sh
phux plugin install someone/phux-plugin
phux config reload
```

`someone/phux-plugin` is shorthand for
`https://github.com/someone/phux-plugin`. A plugin that lives in a
subdirectory is `someone/dotfiles/plugins/usage`, or any git URL with
`--subdir`. `phux config reload` makes the plugin's actions, keys, panes,
widgets, and sidebar sections appear in attached clients.

**A plugin runs as you.** phux never loads plugin code into the server,
and every plugin command runs as a separate process, but that is not a
sandbox. A plugin's commands, and the build steps `phux plugin install`
and `phux plugin update` run, can do anything your user can. Read a
plugin before you install it.

## Managing plugins

Plugins are listed in your [configuration file](./config.md) as
`[[plugins]]` entries. The `phux plugin` commands edit those entries for
you and never touch a running server:

| Command | Does |
| --- | --- |
| `phux plugin install REF` | Fetches GitHub `owner/repo[/subdir]`, a git URL, a local directory, or a tarball, runs its build steps, validates it, and links it |
| `phux plugin update [NAME]` | Re-fetches, rebuilds, and revalidates installed plugins, or just NAME |
| `phux plugin new NAME` | Writes a minimal plugin: a manifest, one action, one hook, a script, and a README |
| `phux plugin link PATH` | Adds a manifest already on disk, such as one you're writing |
| `phux plugin unlink ID` | Removes a plugin's entry |
| `phux plugin enable ID` | Turns a plugin on |
| `phux plugin disable ID` | Turns a plugin off and keeps its entry |
| `phux plugin list` | Lists configured plugins and whether each is enabled |
| `phux plugin validate [PATH]` | Checks one manifest, or every configured one |
| `phux plugin log` | Shows recent plugin action and hook runs |

```sh
$ phux plugin list
com.phux.usage 0.1.0 (enabled)
com.phux.demo.theme-pack 0.1.0 (disabled)
```

`install` copies the plugin into `$XDG_DATA_HOME/phux/plugins` (or
`~/.local/share/phux/plugins`) and records where it came from, so
`update` can fetch it again. `--rev` installs a branch or tag of a git
URL or of `owner/repo` shorthand, and `--disabled` links it turned off.
A plugin can require a minimum phux version; installing it on an older
phux fails with a message saying so.

### Where `install` looks

`REF` is chosen in this order:

1. An existing local directory is copied. An existing local file whose
   name ends in `.tar`, `.tar.gz`, or `.tgz` is extracted. Any other
   existing file is an error.
2. A git URL (`https://…`, `git@…`, `file://…`) is cloned as given, with
   the system `git`. The URL is not rewritten. `--subdir PATH` uses the
   plugin in that subdirectory of the clone instead of the repository
   root.
3. `owner/repo` or `owner/repo/sub/dir` is GitHub shorthand. phux clones
   `https://github.com/owner/repo` and, when more path segments follow
   the repo, uses them as the subdirectory. A trailing `.git` on the
   repo name is ignored. Nothing is contacted except that clone.

An existing local path wins over shorthand. `./foo/bar`, `../foo`, an
absolute path, a `~` path, a trailing slash, and a reference with a `.`
or `..` segment are always local paths, never shorthand. When a reference
matches both an existing path and `owner/repo`, the message says which
one was chosen. `--subdir` on a reference that already names a
subdirectory is an error, and the message names both.

```sh
phux plugin install someone/phux-plugin --rev v1.2.0
phux plugin install someone/phux-plugin --rev 0123456789abcdef0123456789abcdef01234567
phux plugin install https://github.com/someone/plugins --subdir plugins/usage
```

`--rev` is a branch, a tag, or a full 40-character commit SHA. A SHA is
fetched and checked out, and that commit stays pinned. A password or
token in a URL (`https://user:token@host/...`) is removed before the
address is stored or printed; the clone itself still uses the URL you
typed. For a private repository, use a git credential helper rather than
putting a token in the URL. A git install or update whose manifest has
`[[build]]` steps prints those commands and asks `[y/N]` on a terminal.
Pass `--yes` to run them without asking. Without a terminal, the error
names `--yes` and lists the steps it refused. `update` stores a hash of
the approved build commands (the argv of each step), not of the files
those commands run. The same argv is not asked again: a changed script
behind an approved command, such as `build.sh` after `sh build.sh` was
approved, is not re-asked. That is the same trust model as install. A
hash written by an older phux does not match, so `update` asks again.
Control characters in a step are shown escaped, in the prompt and in the
refusal, so that text is the command that will run. Refusing on install
installs nothing. Refusing on update leaves the installed copy in place.

### What install and update trust

`install` and `update` fetch with the system `git`: a shallow clone of a
branch or tag, or a fetch of one commit when `--rev` is a SHA. The only
protocols git is allowed are `https`, `ssh`, `git`, and `file`. `file` is
for a repository already on disk, including tests. GitHub shorthand is
`https://github.com/owner/repo` and nothing else is contacted. Each git
command waits at most 180 seconds (`PHUX_PLUGIN_GIT_TIMEOUT_MS` is that
limit in milliseconds). If `GIT_SSH_COMMAND` is set, that command is
kept and phux appends `-o BatchMode=yes`, unless it already sets
`BatchMode` or the program is PuTTY `plink` or `tortoiseplink` (those
do not take OpenSSH `-o` options, so the command is left unchanged). If
`GIT_SSH` or git `core.sshCommand` is set, `BatchMode` is not forced, so
a password prompt can wait until the 180 second clone timeout. The
resolved commit SHA is written to `plugins.lock`. Two installs at once can lose one of those lockfile
writes; the file is not locked.

`update` moves a branch only when the new commit is that recorded SHA or
a descendant of it, looking 100 commits back. A force-moved or rewritten
branch is refused, including when the real history is deeper than that
window. `--latest` accepts the rewritten history. A commit pinned with
`--rev` does not move unless you pass `--latest`, which then follows the
repository's default branch.

A symlink anywhere in the repository root refuses an install of that
root. An install from a subdirectory checks that subdirectory instead.

A plugin is not reviewed. It runs as you, unsandboxed: its commands and
its build steps can do anything your user can. Read it before you
install it.

`link` takes a manifest path, or a directory holding one. The entry it
writes looks like this, and you can write it by hand:

```toml
[[plugins]]
manifest = "~/code/my-plugin/phux-plugin.toml"
enabled = true
```

A relative `manifest` path is relative to the config file that names it.

**Hooks need a server restart.** Enabling, disabling, or updating a
plugin reaches the palette, keys, panes, widgets, and sidebar on
`phux config reload`. Its `[[events]]` hooks run in the server, which
reads them at start; run `phux upgrade` to restart it in place.

## Writing a plugin

`phux plugin new NAME` writes a new plugin directory. `NAME` is the
plugin id (`hello`, `com.example.tools`) and the manifest's `name`.
Without `--dir`, the directory is `./NAME`. `min_phux_version` is the
version of the phux that wrote the file. The directory holds one `hello`
action that runs `scripts/hello.sh`, one `pane-exit` hook, and a README.
A directory that already has files is left untouched.

```sh
phux plugin new hello
phux plugin validate hello
phux plugin link hello
```

A minimal plugin is a manifest and the scripts it runs:

```toml
# phux-plugin.toml
id = "example.pane-tools"
name = "Pane Tools"
version = "0.1.0"
min_phux_version = "0.0.2"
description = "Summarize the focused pane."

[[actions]]
id = "summarize"
title = "Summarize pane"
contexts = ["pane"]
command = ["python3", "summarize.py"]
keys = "t s"

[[events]]
id = "log-exit"
title = "Log pane exits"
on = "pane-exit"
command = ["sh", "-c", "echo \"$PHUX_EXIT_CODE\" >> exits.log"]
```

Link it while you work on it, check it, then reload:

```sh
phux plugin link ~/code/pane-tools
phux plugin validate ~/code/pane-tools
phux config reload
```

`id` must be unique across your plugins. `command` is always an argument
list, run directly and never through a shell unless you call one, as
above. Commands run from the plugin's own directory, so relative paths
like `summarize.py` work.

### What a manifest can declare

| Table | Adds |
| --- | --- |
| `[[actions]]` | A command in the palette. `keys` binds it in the prefix table (`"t s"` is <kbd>C-a</kbd> <kbd>t</kbd> <kbd>s</kbd>); a key that is already bound, by you or by the shipped defaults, keeps its binding and the plugin's is dropped. |
| `[[events]]` | A hook: `on` names a [hook event](./hooks.md#events), and `command` runs every time it fires, with the same environment as a config hook. An `on` that is no hook event would never fire: `phux plugin validate` rejects it, suggesting the nearest event, and the server skips it with a warning. |
| `[[panes]]` | A program to open as a pane. `placement` is `split`, `tab`, `zoomed`, or `overlay` (a floating box). |
| `[[widgets]]` | A status-bar widget: `id`, a `slot` (`left`, `center`, or `right`, the default), and a `kind` with its [options](./reference/widgets.md). It goes after your own widgets. |
| `[[sidebar]]` | A fixed section between Agents and Sessions: `title`, a `format` for each pane's row, and `rows` (1 to 8, default 3). |
| `[[themes]]` | A [theme](./themes.md): a `name` and the `path` of a directory holding `colors.toml`. |
| `[[agents]]` | An agent the plugin integrates, with a `label`, a declared `state`, and an `attention` level. |
| `[[build]]` | A `command` that `install` and `update` run to build the plugin. |

`platforms = ["linux", "macos"]` limits the plugin, or one entry, to those
systems.

A sidebar section's `format` uses `{agent}`, `{cwd}`, `{exit}`,
`{index}`, `{state}`, `{title}`, and `{window}`. A pane gets a row only
when every token in the format has a value for it:

```toml
[[sidebar]]
id = "exits"
title = "Last exit"
format = "{window}: exit {exit}"
rows = 3
```

### What a plugin process receives

A plugin command is a normal child process. It is not sandboxed, and it
inherits the environment of the process that starts it. phux adds the
variables below. These are the ones set today.

**Actions** (`[[actions]]`, from the palette, a key, or
`phux config run`) run with the plugin directory as the working
directory. phux adds:

| Variable | Value |
| --- | --- |
| `PHUX_PLUGIN_ID` | The plugin's manifest `id` |
| `PHUX_PLUGIN_ACTION_ID` | The action's `id` |
| `PHUX_PLUGIN_ROOT` | The plugin directory |

An action does not get `PHUX_SOCKET` from phux. If the process that runs
the action already has `PHUX_SOCKET`, the action inherits it.

**Event hooks** (`[[events]]`) run with the plugin directory as the
working directory. They receive the same variables as a
[config hook](./hooks.md#what-the-command-gets), plus:

| Variable | Value |
| --- | --- |
| `PHUX_PLUGIN_ID` | The plugin's manifest `id` |
| `PHUX_PLUGIN_EVENT_ID` | The hook's `id` |
| `PHUX_PLUGIN_ROOT` | The plugin directory |

Every hook also gets `PHUX_EVENT` (the event name) and `PHUX_SOCKET`
(the server's socket) when the process that fires the hook knows that
socket. `phux worktree new` fires `worktree-created` from the CLI and
sets `PHUX_SOCKET` to the socket that command is using. Each context key
is an environment variable: hyphens become underscores and the name is
uppercased with a `PHUX_` prefix, so `exit-code` is `PHUX_EXIT_CODE` and
`terminal-id` is `PHUX_TERMINAL_ID`. The keys for each event are listed
in [Hooks](./hooks.md#events).

**Panes** (`[[panes]]`) are ordinary panes whose working directory is the
plugin directory. The spawn adds `PHUX_PLUGIN_ID`, `PHUX_PLUGIN_ROOT`,
and `PHUX_PLUGIN_PANE_ID`. The server also sets `PHUX_SOCKET` to its
socket, `PHUX_TERMINAL_ID` to that pane's id, `TERM` from `defaults.term`,
and `COLORTERM=truecolor`. It removes `PHUX_UPGRADE_SOURCE_EXE`,
`PHUX_UPGRADE_SNAPSHOT_DIR`, and any `CLAUDE_CODE_*` or `CLAUDECODE`
variable inherited from the server, so a nested server or `claude` does
not reuse the outer session.

**Build steps** (`[[build]]`, run by `install` and `update`) use the
plugin directory as the working directory and receive `PHUX_PLUGIN_ID`
and `PHUX_PLUGIN_ROOT`. Each step is killed after five minutes.

An `exec` status widget contributed by a plugin is run by the client
with no plugin variables and not in the plugin directory. Themes and
sidebar sections do not start a process.

### Running actions

An action runs from the command palette, from the key in its `keys`, or
from a shell:

```sh
phux config run example.pane-tools summarize
```

To give a plugin action a key, set `keys` in its manifest, or bind it in
your own `[keybindings]` with the `plugin-action` action, naming the
plugin and the action's `id`:

```toml
[keybindings.prefix-table]
"Y" = { action = "plugin-action", plugin = "example.pane-tools", id = "summarize" }
```

Your bindings win over a manifest's `keys`. Binding the same key yourself
takes it back, and a key you set to `"none"` stays unbound even when a
plugin's `keys` names it.

`phux config run` prints the action's output and exits with its exit
code. `--timeout SECS` gives up after that long, and `--json` prints the
result as a JSON document.

## Listing a plugin

Add the GitHub topic `phux-plugin` and a `phux-plugin.toml` at the root of
the default branch that `phux plugin validate` accepts. The static index
is built from that public topic and from the plugins in this repository.
A repository with several plugins lists their directories in
`phux-plugin-dirs.toml`:

```toml
subdirs = ["plugins/pane-tools"]
```

Plugins run as you and aren't sandboxed (see the top of this page). The
index document is [plugin-index.schema.json](spec/plugin-index.schema.json).

## First-party plugins

The optional [first-party plugins](../plugins/README.md) live under `plugins/`:

- `usage` publishes your AI plan limits to every client, from hooks on
  `client-attached` and `agent-state-changed`.
- `continuum` saves and restores your workspace.
- `agent-tools` launches and drives a bench of coding agents.

`phux config init --distro starter` writes a config that turns on
`continuum`, `agent-tools`, and `usage` from a phux source checkout.

For a minimal plugin to copy, see the working
[theme example](../examples/plugins/theme-pack/README.md).

## Debugging

- `phux plugin validate` checks every configured manifest and says what
  is wrong with one that fails, including an event hook that can never
  fire.
- `phux plugin list` shows which plugins are enabled.
- `phux config keys` lists plugin keys with `plugin <id>` as their source.
- `phux plugin log` lists the newest runs of every action and hook, oldest
  first, with exit status and duration. `--failed` shows only failures,
  with the end of their output; `-n` sets how many runs.

```text
$ phux plugin log --failed
2026-10-21 22:34:06  action  example.pane-tools/summarize  exit 1  79ms
    summarize.py: no pane text
```

If an action is missing from the palette, check that the plugin is
enabled and run `phux config reload`. If its hook never shows up in the
log, the server hasn't restarted since you enabled it.
