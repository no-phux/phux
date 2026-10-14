---
audience: humans, contributors, agents
stability: evolving
last-reviewed: 2026-10-13
---

# Glance, session, detail

**TL;DR.** Every human surface that lists work answers at three depths. The
glance (`phux ls`, the session picker, the sidebar) gives one line per session
and leads with what needs a person. The session view (`phux ls SESSION`)
lists that session's terminals, exceptions first. The detail view (`phux ls
-l`, `phux resource show`) shows keys, revisions, activations, and lifetimes.
`--json` always carries everything.

## Why

Agents run at scale; a person reads groups, outcomes, and exceptions. A
listing that prints every record field for every session reads the same at
rest as under load, so it says nothing at a glance. The sidebar already
works this way ([ADR-0089](../adr/0089-three-zone-attention-sidebar.md)):
detail decays with distance, and only urgency crosses that boundary. The CLI
and the session picker follow the same rules.

## Glance

One line per session: its name, how many terminals run, and only the
exceptions a person has to act on.

```text
$ phux ls
api: needs you (1 agent waiting, 1 failed), 5 running
build: 12 running (1 client attached)
scratch: empty, ephemeral
+37 quiet sessions; `phux ls -l` lists every session
```

- **Exceptions**, the "needs you" list: an agent blocked on a person or
  asking for attention, a run that exited with an error or never started, a
  run a server restart lost track of. A session with any of them sorts
  first.
- **Quiet facts** follow: running, not started, or done when nothing runs.
- **Lifetime** shows only as `ephemeral`, because that session vanishes with
  its last terminal. A persistent session is the default and says nothing.
- **The Inbox** shows only while it holds imported work, as `N to organize`.
- **Keys, revisions, window counts, and activation ids** never appear here.
- **Volume is bounded.** Past 20 lines, exception sessions fill the screen
  first and every other session collapses into one `+N quiet sessions`
  line. A thousand sessions print about twenty lines. On a federation hub
  each satellite's sessions collapse the same way under its own header;
  its terminals are left to `phux ls -l`.
- **Cost stays small too.** The glance reads the server's state and the
  organization records once, and the agent records pipelined, one round
  trip per 64 terminals rather than one each. It skips the layout reads
  that only the window counts of `-l` need.

The TUI session picker uses the same order: the current session, then the
sessions that need you with their count, then the rest by name. A picker
row shows only that count. The sidebar's Agents section lists only the
agents that need you and disappears when none do.

## Session

`phux ls SESSION` lists that session's terminals, exceptions first, one line
each: selector, state, the agent and its state when one runs there, then the
title and working directory.

```text
$ phux ls api
api: needs you (1 agent waiting, 1 failed), 5 running
  @9   failed (exit 1)             cargo test       ~/src/api
  @7   claude blocked              review           ~/src/api
  @3   running                     zsh              ~/src/api
```

Selecting a session in the TUI switches to it; the sidebar's `here` zone is
its session view.

## Detail

`phux ls -l` lists every session, uncollapsed, with its window count,
lifetime, and key (`^ab12cd`, the shortest unique prefix that `phux rm` and
`phux relaunch` accept). `phux ls -l SESSION` adds each terminal's key,
record revision, and current activation with its outcome. `phux resource
show TARGET` is the one-terminal record.

## Reports

Bulk verbs report the way the glance does. `phux kill SESSION` and `phux
restore` print one summary line, plus one line for each target that did not
go as asked. `-v` prints every target.

```text
$ phux kill work --yes
killed 12 terminals in work
```

## Machine readers

`phux ls --json` and the bulk verbs' `--json` documents are unchanged by any
of this: complete, versioned, and stable (`schema_version` 4 for `ls`).
Scripts read JSON, not the human text.
