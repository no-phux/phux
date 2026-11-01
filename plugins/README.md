# First-party plugins

Optional functionality implemented as external commands, not code loaded into
the server. The [starter distro](../distros/starter/README.md) enables this set:

| Plugin | Purpose |
| --- | --- |
| [agent-tools](./agent-tools/README.md) | Agent launch templates, pane identity, and integration tools. |
| [continuum](./continuum/README.md) | Save and restore workspace archives, not running processes. |
| [usage](./usage/README.md) | Publish vendor plan limits; reads local sign-ins and contacts only their vendors. |

[nvim](./nvim/README.md) is first-party too, but the starter does not enable it:
one Neovim behind many panes, with `:split` opening a real pane. Install it
with `phux plugin install no-phux/phux/plugins/nvim`.

Keep `plugins/` beside `distros/` when copying the bundle. Existing configs
extending the starter layer pick up its manifest paths automatically. Plugin
IDs are unchanged, including the historical
`com.phux.demo.*` IDs used by key bindings and saved configuration.

## Moving from examples/plugins

Pause plugin actions before migrating. Updating a checkout moves code, not
plugin-local state. Old `state/` directories may remain under
`examples/plugins/continuum` and `examples/plugins/agent-tools`. Keep them:
Continuum archives, integration records, and bench targets are not disposable.

Copy each old `state/` directory to its new plugin directory, from the repo
root. This leaves the source untouched and refuses an existing destination:

```sh
python3 - examples/plugins/continuum/state plugins/continuum/state <<'PY'
import shutil, sys
shutil.copytree(sys.argv[1], sys.argv[2], symlinks=True)
PY
```

Repeat for `agent-tools` if it has state. If the destination already exists,
do not overwrite or delete either tree. Compare them first, or point the
plugin at the retained state explicitly in the environment that launches
phux (restart the server for hook changes):

| Variable | Retained data |
| --- | --- |
| `PHUX_CONTINUUM_DIR` | Directory containing profile archive JSON files. |
| `PHUX_AGENT_TOOLS_STATE_DIR` | Agent-tools `state/integrations` directory. |
| `PHUX_AGENT_BENCH_STATE_DIR` | Agent-tools `state` directory containing bench tables. |

Configs that directly name old manifests must change `examples/plugins/`
to `plugins/`; configs extending starter need no path edit. Run
`phux config check` before reloading.

### Copies installed with plugin install

A managed copy keeps running, but its `plugins.lock` still records the old
local source. Before `phux plugin update`, back up the managed plugin
directory to a **new destination outside the managed tree**, using the same
copy procedure above. Point its state variables at that backup: the updater
replaces the managed package, including plugin-local files.

In `$XDG_DATA_HOME/phux/plugins/plugins.lock` (default
`~/.local/share/phux/plugins/plugins.lock`), change only the affected
`source = "dir"` entries' `ref` paths to the corresponding absolute
`plugins/` source directories. Keep IDs, config entries, and unrelated
provenance unchanged. Then run `phux plugin update ID --json` and verify
the result. Do not reinstall over or delete an existing managed directory.
Keep the backup until archive restoration and integration status are checked.

Small teaching programs live in [examples](../examples/README.md). A no-op
manifest used to test the config loader lives in that crate's test fixtures;
it is not an installable plugin.
