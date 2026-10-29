# Bring your tmux keys to phux

`phux config import-tmux` reads an already running tmux and generates a
native configuration layer for phux's terminal UI. It brings over common
bindings and a few preferences, with a report of what changes or cannot be
represented. Desktop shortcuts use a separate configuration.

Start tmux with your usual configuration first, then preview:

```sh
phux config import-tmux > tmux-preview.toml
phux config check tmux-preview.toml
```

TOML goes to stdout; the report goes to stderr. For every mapped,
approximate, or unsupported entry together with the generated TOML, use
`phux config import-tmux --json`. Preview never writes phux configuration,
starts a phux server, sources a tmux file, or executes a binding.

To select a particular running tmux server, add
`--tmux-socket /path/to/tmux.sock`. Without it, tmux selects its current
server using its normal environment/default socket rules. The importer
reads global session/window preferences and the effective key tables;
per-session and per-window overrides are not imported. A missing tmux or
server is an error; the importer does not launch one to evaluate your
configuration.

## Apply and undo

After reviewing the preview:

```sh
phux config import-tmux --apply
phux config keys
phux config show --layers
```

This creates `tmux-import.toml` beside the active `config.toml` and prepends
it to `extends`. Existing extended files and your root settings still win.
Your comments and settings are preserved, and the complete merged config
must pass validation before the root file is atomically published. If
validation fails, your root file stays untouched and the new import layer
is removed. An existing import is never overwritten; review/remove the
previous import before repeating `--apply`.

If publishing the root fails, the layer is retained and its path is
reported. This also protects a concurrent external edit that may have
started referencing it; inspect the root and layer before retrying.

To undo, remove `"tmux-import.toml"` from the root `extends` array, then
delete that layer. This restores the previous layering. You can also
save a standalone layer with `--output PATH`; that refuses to overwrite
any existing file and does not edit the active config. `--apply` and
`--output` cannot be combined.

Applying saves configuration; it does not signal a reload or restart a
server. See [when settings take effect](./config.md#when-a-change-takes-effect):
key bindings and status position reload, mouse capture changes on attach,
and shell/history defaults are read at server start.

## What carries over

The finite translator recognizes supported command forms exactly. Extra
flags, targets, and command chains are not silently discarded.

| tmux setting or binding | phux result |
| --- | --- |
| `prefix` | `keybindings.prefix` |
| `mouse`, `default-shell`, `history-limit` | Corresponding `[defaults]` values; phux's mouse behavior and history byte cap still apply |
| `status-position` | `status.position` |
| `split-window` / `-h` / `-v` | Split with the same divider direction |
| `select-pane -L/-R/-U/-D`, `last-pane`, `select-pane -t .+` / `.-` | Directional focus or pane cycling |
| `resize-pane -L/-R/-U/-D [N]`, `resize-pane -Z` | Resize by cells, or toggle zoom |
| `new-window`, `next-window`, `previous-window`, `detach-client` | New/next/previous tab, or detach |
| `select-window -t :N` / `:=N` | Tab position after subtracting global `base-index`; gaps in tmux numbering cannot be preserved |
| `switch-client -n/-p/-l` | Next/previous/last session |
| Basic rename prompts, session/window pickers, `list-keys` | Native phux prompts, pickers and help, reported as approximate |
| `command-prompt`, `source-file PATH` | phux's action palette or native config reload, reported as approximate |
| `copy-mode` | Enters phux copy mode; navigation and selection keys differ |

Bindings from tmux's `prefix` and `root` tables become prefix and global
bindings. Repeat bindings are reported as approximate: tmux's repeat
timing is not imported. Key spellings that collapse to one physical phux
key are disabled and reported rather than choosing one arbitrarily.

The layer disables phux's shipped prefix bindings before importing tmux
keys, so a tmux key does not accidentally invoke an unrelated phux
default. Unsupported bindings stay unbound. In phux, an unbound key after
the prefix goes to the pane; this differs from tmux's handling. Existing
phux layers may explicitly restore or replace any binding.

Confirmation is never silently removed. tmux's usual confirmed tab close
maps to phux's own confirmed close. Its confirmed `kill-pane` binding
stays unsupported because phux's `kill-pane` is immediate. A direct,
unconfirmed tmux `kill-pane` can be mapped.

Plugins, hooks, arbitrary shell commands, custom and copy-mode key tables,
status format languages, secondary prefixes, and live sessions/processes
are not migrated. Selected unsupported preferences and every inspected
prefix/root binding appear in the report; other tmux options retain phux
defaults. This is a one-shot conversion, with no tmux dependency at runtime.

New panes use phux's `defaults.cwd-inheritance` policy. tmux's per-command
`-c` working-directory forms are not converted; review these bindings in
the report rather than assuming their directory behavior carries over.

Exit 0 means inspection, translation, validation and any requested save
succeeded; unsupported entries are reported within that successful plan.
Exit 2 means inspection, translation, validation or publication failed.
With `--json`, success is one stdout document (`schema_version`, `toml`,
`entries`, `notes`, `saved`, `applied`); failure uses the standard
[JSON error contract](./scripting.md#json-output-and-errors) on stderr,
with empty stdout.
