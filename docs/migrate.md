# Migrate your config with your agent

`phux migrate` moves the key bindings you already have in tmux, zellij,
Ghostty, herdr or Rex into your phux config. phux reads your files and decides
every line it can; your own coding agent (Claude Code, Codex or OpenCode)
writes the result, checks it and reloads phux, in a pane next to you.

```sh
phux migrate
```

## What it reads

| Source | Files |
| --- | --- |
| tmux | the first of `~/.tmux.conf` and `$XDG_CONFIG_HOME/tmux/tmux.conf` (or `~/.config/tmux/tmux.conf`), and every file it `source`s |
| zellij | `$XDG_CONFIG_HOME/zellij/config.kdl` |
| Ghostty | `$XDG_CONFIG_HOME/ghostty/config` and, on macOS, `~/Library/Application Support/com.mitchellh.ghostty/config`, and every `config-file` they load |
| herdr | `$XDG_CONFIG_HOME/herdr/config.toml` |
| Rex | `$XDG_CONFIG_HOME/rex/init.lua` (not the files it `require`s) |

`--source tmux` (repeatable) reads only the sources you name. With nothing
to migrate, `phux migrate` says so and exits 0.

phux only reads files. It never runs tmux or any other program, and it never
edits your phux config itself: the agent does, and it backs up an existing
file first.

## Two questions

When it reads a tmux config, `phux migrate` asks two questions before it
starts the agent. Enter takes the default.

1. **Setup mode.** Beginner (the default) keeps phux's defaults wherever they
   already do what your tmux did, and writes only real differences. An
   `unbind '"'` beside your own `bind - split-window -v` is not written:
   phux already ships <kbd>-</kbd> as that split, so removing <kbd>"</kbd>
   would only lose a key.
   Advanced mirrors tmux as closely as phux allows: it writes your tmux
   unbinds, and tmux's prefix becomes the default answer to the next
   question. Your tmux bindings win over phux's defaults in both modes.
2. **Prefix.** phux's prefix is <kbd>C-a</kbd>. When your tmux prefix is
   different (tmux's own <kbd>C-b</kbd>, or your `set -g prefix`), keep
   phux's or use tmux's.

`--advanced` and `--prefix C-b` answer for a script. Without a terminal, and
with `--print` or `--json`, phux takes the defaults: beginner, phux's prefix
(tmux's with `--advanced`).

## What phux decides, and what the agent decides

For tmux and Ghostty, phux writes a table with one row per line of your
files: the exact phux line to write, "already phux's default", or "not
written" with the reason. tmux bindings use the same translator as
[`phux config import-tmux`](./tmux-import.md); `-c "#{pane_current_path}"`
is dropped because phux opens new panes in the focused pane's directory
already. tmux `mouse`, `history-limit`, `default-shell`, `status-position`
and `set-clipboard` become `[defaults]` and `[status]` settings, written only
where they differ from phux's. `set-clipboard off` becomes
`defaults.clipboard-write = "deny"`;
`on` keeps phux's default `"allow"`. `external` becomes `"deny"` too:
in the TUI, pane programs cannot write the host clipboard, but manual copy
still works because it bypasses the pane-write policy. This guarantee is
TUI-specific: Rune's current pane clipboard path does not enforce this setting.
A trailing flag-free `display`/`display-message` with one literal message
is cosmetic and can be dropped. Flags, targets and formats are not dropped.
Ghostty keybinds become [global bindings](./keys.md#binding-a-key).

Some lines are never written:

- Terminal settings: font, colours. A Ghostty setting keeps working in
  Ghostty.
- A theme line. When you already picked a phux theme (`phux theme set`, or
  `[theme] name` / `file` in your config), the line is "nothing to do" and
  reads "kept your phux theme"; otherwise it goes under "Could not migrate"
  and points you at `phux theme list` and `phux theme set`.
- Any binding on Cmd. phux key bindings have no Cmd modifier. A Ghostty Cmd
  binding keeps working, but on Ghostty's own tabs and splits, not phux's.
  The macOS
  system shortcuts (<kbd>Cmd</kbd>+<kbd>Q</kbd>, <kbd>Cmd</kbd>+<kbd>H</kbd>,
  <kbd>Cmd</kbd>+<kbd>M</kbd>, <kbd>Cmd</kbd>+<kbd>Tab</kbd>,
  <kbd>Cmd</kbd>+<kbd>Space</kbd>, <kbd>Cmd</kbd>+<kbd>Shift</kbd>+<kbd>3</kbd>/<kbd>4</kbd>/<kbd>5</kbd>,
  <kbd>Ctrl</kbd>+<kbd>Cmd</kbd>+<kbd>Q</kbd>) are never bound.
- tmux copy-mode bindings, plugins and commands no phux action matches.

zellij, herdr and Rex lines, and Ghostty lines phux cannot translate with
certainty (key sequences, actions with no clear counterpart), go to the
agent as "translate these yourself", with the list of phux actions it may
bind. A binding with no phux action is reported, never approximated.

When two sources bind the same key differently, the agent asks you once per
pair of sources which one wins. It never decides alone.

The agent then runs `phux config check` until the file is clean, `phux
config reload`, and `phux config keys --json`. It then checks intent, not
just syntax: every binding you had is on its key in phux and does the same
thing (written or already the default), no key it removed is one your
config still binds, and no tmux line was left out that a phux action does
exactly. It reports five lists:
Migrated, Replaced phux defaults, Conflicts between sources, Nothing to do,
and Could not migrate. Every line of your files lands in one of them.
"Nothing to do" is grouped by reason: terminal settings that keep working
in the terminal phux runs in (a Ghostty font, opacity or shader), and tmux
options phux already behaves like (`mode-keys vi`, `renumber-windows on`,
`escape-time 10`). Other Escape timings, including `0` and `500`, are not
migrated: phux uses a fixed 10 ms wait.
"Could not migrate" is what you have to act on, one line each: a tmux
`base-index 1` (phux numbers tabs from 0),
status-bar formats, plugins. If its sandbox blocks
the reload, it asks you once, or tells you the reload is pending.

## Where the agent runs

- `--agent claude|codex|opencode` picks the agent. Without it, phux starts the
  one that is installed, asks when there are several, and with `--json` or no
  terminal fails with the list.
- Inside a phux pane, the agent opens in a split to the right.
- Anywhere else, it starts in a new session named `migrate` and attaches.
  Without a terminal, phux prints the `phux attach` command instead.

The agent's working directory is your phux config directory, created if
needed.

## Any other agent

`phux migrate --print` prints the prompt and starts nothing. Paste it into
any agent:

```sh
phux migrate --print | pbcopy
```

## JSON

`phux migrate --json` prints one document after launching:

```json
{
  "schema_version": 1,
  "sources": [
    {
      "name": "tmux",
      "files": ["/Users/me/.tmux.conf", "/Users/me/.tmux/keys.conf"],
      "found": { "bindings": 13, "unbindings": 1, "settings": 5 }
    }
  ],
  "agent": "claude",
  "config_path": "/Users/me/.config/phux/config.toml",
  "theme": "tokyo-night",
  "launched": { "pane": "@12" },
  "prompt_bytes": 15873,
  "prefix": { "chosen": "C-a", "tmux": "C-b", "phux": "C-a" },
  "mode": "beginner"
}
```

- `sources[].name` is `tmux`, `zellij`, `ghostty`, `herdr` or `rex`; `files` are
  absolute; `found` counts binding, unbinding and setting lines.
- `theme` is the phux theme your config selects (the name, or the file path
  for `[theme] file`), or `null` when none is selected.
- `launched` is `{ "pane": "@N" }` for a split, `{ "session": "migrate",
  "pane": "@N" }` for a new session, and `null` when nothing started.
- `mode` is `beginner` or `advanced`.
- `prefix.chosen` is the prefix the migrated config ends up with,
  `prefix.tmux` tmux's (`null` without a tmux config), `prefix.phux`
  phux's own.
- With nothing to migrate, `sources` is empty, `agent` and `launched` are
  `null`, and `prompt_bytes` is 0.
- `--print --json` starts nothing: `launched` is `null`, `agent` is the one
  `--agent` names or the only one installed (else `null`), and the document
  adds `prompt`, the full text.

Failures use the [JSON error contract](./scripting.md#json-output-and-errors):
`usage_error` (exit 2) when the agent is not installed, none is, or several
are and `--agent` is missing, or when `--prefix` is not a phux key.
