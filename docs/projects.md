# Projects

A project is a repository checkout with its own phux session. A
`.phux/project.toml` recipe in the repository describes the tabs, panes,
and commands that session starts with, so `phux project open` gives you
the same workspace every time, on any machine.

```sh
phux project init     # write a starter .phux/project.toml
phux project open     # build the session from it, or attach if it's running
```

phux runs a recipe only after you approve its exact contents on this
machine.

## Writing a recipe

`phux project init` writes this starter into the current checkout:

```toml
[[windows]]
name = "code"
panes = [
  { id = "editor", command = ["sh", "-c", "exec ${EDITOR:-vi} ."] },
  { split = { target = "editor", direction = "right", ratio = 0.6 } },
]

[[windows]]
name = "shell"
```

Each `[[windows]]` entry is a tab, in order. Each pane in `panes` is one of:

- The window's first pane, which needs no `split`.
- A later pane, whose `split` places it beside an earlier pane in the
  same window: `target` is that pane's `id`, `direction` is `right` or
  `down`, and `ratio` (between 0 and 1) is how much the target keeps.

| Key | Where | Means |
| --- | --- | --- |
| `name` | window | The tab's name |
| `id` | window, pane | A name for `focus` and `split` to refer to: letters, digits, `-`, `_` |
| `command` | pane | An argument list to run; leave it out for a plain shell |
| `exec` | pane | `true` closes the pane when `command` exits; by default you drop to a shell |
| `cwd` | window, pane | The working directory, relative to the checkout |
| `env` | top level, window, pane | Environment variables; inner tables add to outer ones |
| `focus` | top level | `"WINDOW.PANE"` ids of the pane to focus; default is the first |

A recipe with no windows opens one shell at the checkout root. Commands
are argument lists and never pass through a shell, unless you run one, as
the editor pane above does. A recipe can't set `PHUX_*` variables.

phux checks the whole recipe before it starts anything: an unknown key, a
`split` target that doesn't exist, a missing directory, or more than 64
panes refuses the recipe with a message naming the window and pane.

## Opening a project

```sh
phux project open             # the checkout you're in
phux project open ~/code/site # another checkout
phux project open phux        # a named project from your config
```

Inside a git repository, phux opens the checkout's root. If the project's
session is already running, `open` attaches to it as it is and never
reruns the recipe. Otherwise it builds the session from the recipe, then
attaches.

`--json` builds the session without attaching and prints its name and
the first pane, for scripts.

## Trust

A recipe can run any program, and it arrives with the repository, so
phux won't run one you haven't approved:

```sh
$ phux project trust
Trusted /Users/you/code/site/.phux/project.toml (sha256 9f2c...).
```

An approval covers the recipe's exact bytes in that repository, on this
machine, in every worktree of the repository. **Changing one byte of the
recipe withdraws the approval.** After you edit it, or pull someone
else's edit, read it and run `phux project trust` again.

If you `open` a recipe that isn't approved, phux prints it and asks
before running it. Without a terminal to ask on, or with `--json`, it
refuses and tells you to run `phux project trust`.

| Command | Does |
| --- | --- |
| `phux project trust [TARGET]` | Validates the recipe and approves its current bytes |
| `phux project untrust [TARGET]` | Withdraws the approval |
| `phux project status [TARGET]` | Prints `trusted`, `untrusted`, or `absent`; exits 0 when trusted, 1 when not, 2 when it can't tell |

`phux project init` approves the starter it writes, so a fresh recipe
opens straight away until you edit it.

## Named projects

Name your checkouts in the [configuration file](./config.md) to open them
from anywhere:

```toml
[[projects]]
name = "phux"
path = "~/code/phux"

[[projects]]
name = "site"
path = "~/code/site"
recipe = "~/.config/phux/recipes/site.toml"
```

`path` is absolute or starts with `~/`; phux never scans for
repositories. `recipe` points at a recipe kept outside the repository,
used instead of the checkout's own. It needs no approval, because you
wrote it into your own config.

```text
$ phux project list
phux                 open     ~/code/phux
site                 -        ~/code/site
```

The second column is `open` when the project's session is running,
`missing` when the path doesn't exist, and `?` when no server answered.

## Grouping sessions

The sidebar, the session picker, and `phux ls` group sessions by project.
A session created inside a git checkout joins its repository's group,
across machines. Click a group's header in the sidebar to collapse it.

To put any other session in a project, link it by name. The link is
stored on the server that owns the session, so every client sees it:

```sh
phux project link notes phux    # put session "notes" in the phux group
phux project unlink notes       # back to its repository, or ungrouped
```

A link outranks the repository, and a link named like a repository joins
that repository's group. `--remote me@mini` links a session on another
machine.

## Debugging

- `phux project status` says whether the recipe is approved.
- `phux project trust` validates the recipe and reports the first problem
  without starting anything.
- `phux project list` shows your named projects and which are open.
- `phux project inspect` maps a repository's worktrees to their checkouts.

If `open` attaches to a session that doesn't match your edited recipe,
the session was already running: recipes only build new sessions. Close
the session and open it again to rebuild it.
