# Rune upstream follower

An OpenCode agent whose only job is keeping [Phux Rune](../../clients/rune/README.md)
on the newest [Rune](https://github.com/anomalyco/rune) `dev`: bump the pin,
adapt to API and behavior changes, adopt new upstream features, prove it, and
merge it. It runs twice a day in its own phux session, where anyone attached
can watch and steer it.

| Piece | What it is |
| --- | --- |
| [`.opencode/agents/rune-upstream.md`](../../.opencode/agents/rune-upstream.md) | The agent: its mission and rules, loaded from the worktree it runs in. It may run anything except the few commands that would move `main` around a PR |
| [`tick.sh`](./tick.sh) | One scheduled tick: heal the session and worktrees, start a fresh run unless one is busy |
| [`install.sh`](./install.sh) | The home directory and a LaunchAgent that runs `tick.sh` |

## Install

On the Mac that hosts it, from a checkout of `main`:

```sh
scripts/rune-upstream/install.sh              # ticks at 07:00 and 19:00
scripts/rune-upstream/install.sh --hours "6 18"
launchctl kickstart "gui/$(id -u)/sh.phux.rune-upstream"   # tick now
scripts/rune-upstream/install.sh --uninstall
```

The home defaults to `rune-upstream` beside the primary checkout:

| Path | Holds |
| --- | --- |
| `phux/` | The worktree a run starts in and loads the agent from, on a clean, detached `origin/main`. Nothing is edited here |
| `worktrees/` | One linked worktree per change (`rune-upstream-abc1234`, `rune-release-VERSION`), removed once its PR merges |
| `opencode.jsonc` | Makes `rune-upstream` the default agent for OpenCode under the home only |
| `STEERING.md` | Standing orders, read first on every run |
| `JOURNAL.md` | The agent's memory between runs, and the first place a `BLOCKED:` line shows up |
| `rune.git` | A mirror of upstream for reading ranges |
| `scratch/` | Snapshots and logs a run makes |
| `tick.log` | What each tick did |

Rerun `install.sh` after `tick.sh` changes; it keeps the worktree, journal and
steering file.

## A tick

`tick.sh` makes sure the server is up and the worktree and session exist. If
the pane tagged `#rune-upstream-run` is still `working` or `blocked` and
younger than `RUNE_UPSTREAM_MAX_RUN_HOURS` (10), it leaves it alone.
Otherwise it closes the old run and opens a new pane running
`opencode --auto --prompt "Scheduled rune-upstream tick ..."` through your
login shell, tagged `#rune-upstream-run`. Each run is a fresh OpenCode
session; earlier ones stay in OpenCode's session list. A pane that crashed,
a session someone deleted, or a start worktree that went missing is rebuilt
on the next tick, and a change worktree whose PR merged is removed. A tick that can't
read the session's state leaves everything alone.

## Steering

```sh
phux attach rune-upstream          # watch the run, type to steer it
phux tag add '#rune-upstream-run' hold      # pause: ticks skip while any pane has #hold
phux tag rm '#rune-upstream-run' hold       # resume
```

Typing into the live run steers that run. An order that should last, such as
`release: auto`, goes in `STEERING.md`; tell the run and it writes it there.

## Steering from another tailnet

The session is ordinary phux, so anyone who can reach this server can attach
to it. A trusted person on another tailnet either gets this machine shared
into their tailnet, or a [relay](../../docs/remote.md#relays) both can reach,
and then a pairing link from `phux pair`. **A paired device controls the
whole server**, every session on it ([Who can do what](../../docs/remote.md#who-can-do-what)).
To give a co-steerer this job and nothing else, run the follower on its own
server: `install.sh --profile rune` runs every tick against the `rune` profile,
and pairing that profile's server shares nothing else.
