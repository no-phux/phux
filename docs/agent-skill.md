# Agent skill setup

`phux skill setup` teaches every installed agent about phux in one step.
`phux skill` itself prints the guide; see
[Teaching an agent phux](./agents.md#teaching-an-agent-phux).

```sh
phux skill setup             # every detected agent
phux skill setup --dry-run   # say what would change
phux skill status            # report, change nothing
phux skill remove            # take out only what setup installed
```

It writes a short skill named `phux` whose whole body says: run
`phux skill` and follow what it prints (or `phux --help` and `phux doctor`
if that fails). The guide therefore always matches the installed binary.
An agent counts as installed when its binary is on `PATH` or its config
directory exists (`~/.claude`, `~/.codex`, `~/.config/opencode`, `~/.pi`).

| Agent | Skill | Hook |
| --- | --- | --- |
| Claude Code | `~/.claude/skills/phux/SKILL.md` | `SessionStart` in `~/.claude/settings.json` |
| Codex, OpenCode, pi | `~/.agents/skills/phux/SKILL.md` (written once) | none |

With no agent found, only `~/.agents/skills/phux/SKILL.md` is written.
`--agent claude|codex|opencode|pi` (repeatable) picks agents, and
`--project` writes into the current repository (`.claude/skills`,
`.agents/skills`, `.claude/settings.json`) instead of your home directory.

The Claude Code hook runs
`/bin/sh -c 'phux skill session-start 2>/dev/null || true'` with a 5
second timeout. It prints one line for the new session: whether it runs
inside a phux pane, which pane (`@N`) and session, and a pointer to
`phux skill`. It always exits 0. The rest of `settings.json` is kept as
it is. The hook is skipped, and the result says why, when the
`install-claude` wrapper or the phux Claude Code plugin already registers
phux hooks.

A `phux` skill that phux did not install is left alone and reported as a
`conflict`; `--force` writes the stub over it. A `using-phux` skill from
`npx skills add no-phux/skills` is reported as `present`, and no stub is
added beside it. Running setup again rewrites a stale stub (`updated`) and
reports the rest as `up_to_date`. `phux doctor` has a `skill` line.

`--json` prints one document for `setup`, `status`, and `remove`, always
listing all four agents in this order:

```json
{
  "schema": "phux.skill-setup/v1",
  "action": "setup",
  "dry_run": false,
  "scope": "user",
  "agents": [
    {
      "agent": "claude",
      "detected": true,
      "skill": { "path": "/Users/me/.claude/skills/phux/SKILL.md", "state": "installed" },
      "hook": { "state": "installed" }
    },
    {
      "agent": "codex",
      "detected": false,
      "skill": { "path": "/Users/me/.agents/skills/phux/SKILL.md", "state": "skipped" },
      "hook": { "state": "skipped", "reason": "phux installs no session hook for codex" }
    }
  ]
}
```

`scope` is `user` or `project`. `hook.reason` appears only with a reason.
The states each action reports:

| Action | `skill.state` and `hook.state` |
| --- | --- |
| `setup` | `installed`, `updated`, `up_to_date`, `conflict`, `present`, `skipped` |
| `status` | `up_to_date`, `stale`, `missing`, `conflict`, `present`, `skipped` |
| `remove` | `removed`, `missing`, `conflict`, `present`, `skipped` |

`skipped` means the agent was not a target (not detected, or not named by
`--agent`), or, for a hook, that something else owns it. A settings file
that does not parse is reported as a hook `conflict` and never rewritten.
With `--dry-run`, the states are what the run would do.
