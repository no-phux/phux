# Distributions

A distro is a portable set of phux defaults, plugins and, optionally, agent
instructions. Your personal config stays on top. Changing distro should not
mean copying someone else's config over yours.

## Choose and switch

```sh
phux config distro list
phux config distro current
phux config distro use crew
phux config reload
# For Crew, open a lead in a visible pane:
phux launch crew
```

`use` also accepts a layer path or a distro directory. It validates the new
stack before writing and preserves comments, personal settings and unrelated
`extends` layers. `off` removes the managed distro selection; it does not reset
your config. The CLI reports what changed and when a reload is needed.

```sh
phux config distro use ~/src/phux/distros/crew/crew.toml
phux config distro use starter
phux config distro off
```

Names are searched in `$PHUX_DISTROS_DIR`, then
`$XDG_DATA_HOME/phux/distros` (or `~/.local/share/phux/distros`), then a dev
binary's source checkout. Keep a distro's dependencies alongside it: the
bundled distros currently reference plugins in the monorepo. A full path works
with an installed binary even when its build-time checkout is gone.

These are local file edits, not commands to stop agents or restart servers.
[Normal application timing](./config.md#when-a-change-takes-effect) still
applies: theme and palette on reload, sidebar on reattach, hooks at server
start. An existing agent conversation keeps its loaded instructions until
you start another conversation. Mission state and Crew homes survive switches.

## Included distros

| Distro | What you get |
| --- | --- |
| `starter` | Workspace autosave/restore, agent launch integrations, usage hooks. |
| `crew` | Starter plus a lead-agent home, ship/scout/pulse skills, fleet board and Crew palette entries. |
| `herdr` | Compatibility name for the starter layer. |

[Crew's guide](../distros/crew/README.md) covers launching a lead, choosing a
harness, personal policy, recovery and the limits of unattended supervision.
It is inspired by Firstmate's agent-distro approach but uses phux's native
missions, panes, lifecycle, worktrees and mail instead of a parallel runtime.

## Write a distro

Make `distros/NAME/NAME.toml` (or put that directory under your distro search
path). It's an ordinary [config layer](./config.md), not a new schema:

```toml
extends = ["../starter/starter.toml"]

[theme]
accent = "#a6f750"

[[plugins-append]]
manifest = "phux-plugin.toml"
```

Relative paths resolve against the layer's directory. Use `plugins-append`
to compose rather than erase other plugin entries. Your own `config.toml`
still wins; an explicit plain `plugins` array can opt out of bundled plugins.
Validate with `phux config check PATH` and `phux plugin validate PATH`.

An agent distro may add instructions and skills, but it must not silently
overwrite global harness settings, change permission policy, copy credentials,
install tools or claim publication authority. Keep mutable operator policy
and execution state outside the shipped bundle. A distro is trusted local
code running as you, not a security boundary.
