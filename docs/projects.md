---
group: customize
order: 7
---

# Projects

How to give a repository a reproducible phux session. A `.phux/project.toml`
recipe describes the tabs, panes, and commands; `phux project open` builds
the session from it, or attaches if it is running. Every subcommand is in the
[CLI reference](./reference/cli.md#phux-project).

```sh
phux project init     # write a starter recipe (and trust it)
phux project open     # build the session, or attach to it
```

## Writing a recipe

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

Each `[[windows]]` entry is a tab. A window's first pane needs no `split`;
each later one splits an earlier pane by `id`, `right` or `down`, with
`ratio` the share the target keeps.

| Key | Where | Means |
| --- | --- | --- |
| `name` | window | Tab name |
| `id` | window, pane | Name for `focus` and `split`: letters, digits, `-`, `_` |
| `command` | pane | Argv to run; omit for a shell |
| `exec` | pane | `true` closes the pane when `command` exits |
| `cwd` | window, pane | Directory relative to the checkout |
| `env` | top, window, pane | Variables; inner tables add to outer ones; no `PHUX_*` |
| `focus` | top | `"WINDOW.PANE"` to focus; default the first |

Commands are argv lists and never pass through a shell unless you run one.
phux validates the whole recipe first: an unknown key, a missing split
target or directory, or more than 64 panes refuses it with the window and
pane named.

## Opening

```sh
phux project open                # this checkout's root
phux project open ~/code/site    # another checkout
phux project open phux           # a named project
```

A running session is attached as it is; a recipe only builds new sessions,
so close the session to rebuild it after an edit. `--json` builds without
attaching and prints the session and first pane.

## Trust

A recipe runs programs and arrives with the repository, so phux runs one
only after you approve its exact bytes, per repository and machine (shared
by its worktrees). **Changing one byte withdraws the approval.**

```sh
phux project trust      # validate and approve the current bytes
phux project untrust
phux project status     # trusted / untrusted / absent; exits 0 trusted, 1 untrusted or absent, 2 when it cannot tell
```

Opening an unapproved recipe prints it and asks. Without a terminal, or with
`--json`, it refuses.

## Named projects

```toml
[[projects]]
name = "site"
path = "~/code/site"                           # absolute or ~/; phux never scans
recipe = "~/.config/phux/recipes/site.toml"    # optional; needs no approval
```

`phux project list` shows each project as `open`, `-`, `missing`, or `?`
(no server answered).

## Grouping sessions

The sidebar, session picker, and `phux ls` group sessions by project. A
session created in a git checkout joins its repository's group, across
machines. Link any other session by name; the server that owns it stores the
link:

```sh
phux project link notes phux    # put session "notes" in the phux group
phux project unlink notes
```

A link outranks the repository. `--remote me@mini` links a session on
another machine. `phux project inspect` maps a repository's worktrees to
their checkouts.
