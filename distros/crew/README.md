# Crew

**One lead. A visible crew. Finished work.**

Crew is phux's first-party agent distro: a portable bundle of lead instructions,
ship/scout/pulse skills, a private coordinator home, and a fleet board. Talk to
one agent; it uses native phux missions to put workers in isolated worktrees
and visible panes, supervises them, checks their evidence and brings back the
result. Inspired by [Firstmate](https://github.com/kunchenguid/firstmate)'s
agent-distro idea, implemented on phux's existing primitives—not a fork or a
second scheduler.

## Get in

Keep a phux source checkout on disk; Crew extends the sibling starter bundle.
Use a phux build with `config distro` and native `mission` support, Python 3.9+
and a supported agent already installed and signed in. Install its
[phux integration](../../docs/agents.md#agent-setup) for lifecycle/mail delivery.

From the checkout:

```sh
phux config distro use ./distros/crew/crew.toml
phux config reload
python3 distros/crew/crew.py doctor --agent opencode
phux launch crew
# Or choose another harness:
phux launch crew -- --agent claude
```

Or open the command palette and select **Crew: open lead**. Both open a new
tab running your agent in its own home. **Crew: fleet board** opens live
mission/agent status. Palette launch defaults to OpenCode; set
`PHUX_CREW_AGENT=claude` (or `codex`, `pi`, `omp`, `grok`) in phux's launch
environment to choose another. Shell launches accept `--agent` explicitly.
Agents keep their own sign-in, model choice and normal permission prompts.
Nothing grants blanket autonomy or installs a global hook.

```text
Ship the flaky login fix in ~/src/my-app. Open a PR; don't merge it.
Scout why CI is slow in ~/src/my-app. Give me evidence, don't change code.
What needs me?
Pick up where we left off.
```

The lead reads project instructions and uses the project's tracker where
required. Native missions are execution records, not a replacement for Beads
or Linear. Small work stays small; a large request gets bounded independent
lanes and fresh review, not a fleet for the sake of a fleet.

## Make it yours

The default home is `$XDG_STATE_HOME/phux/crew`, or
`~/.local/state/phux/crew`. Add preferences to its **POLICY.md**: repositories,
budget, preferred models, publication authority and how you want to hear about
decisions. Crew never overwrites that file.

```sh
# A second independent lead, with a separate policy and conversation location:
python3 distros/crew/crew.py start --home ~/crews/research --agent claude

# Agent arguments are passed literally, never evaluated as shell code:
python3 distros/crew/crew.py start --agent claude -- --model sonnet
# The same launch in a new phux pane:
phux launch crew -- --agent claude -- --model sonnet

# Inspect launch without creating files, starting phux, or contacting a model:
python3 distros/crew/crew.py start --agent codex --print
```

Only one launcher may hold a home. Its OS lock is also inherited by the direct
agent process, so killing the launcher alone does not release a lock the agent
still holds; a stale file is not a stale lock. Another home gives you another lead,
but native missions/mail remain in the selected phux profile's shared store.
This is coordination, **not a security sandbox**. The lead and workers run
as you. Never put a home inside a project or use a checkout as a home.

Managed AGENTS.md and CLAUDE.md point to this bundle; they are never silently
replaced if edited or pointed at a different checkout. Put personal edits in
POLICY.md. Keep the source checkout at its current path (updates to its lead
contract apply next session), or use a new home when moving the bundle.
The launcher starts a new agent conversation; it doesn't promise to resume a
provider conversation. The lead reconstructs execution state from native
missions, mail, branches and artifacts on every boot.

## Switch without wrecking your setup

```sh
phux config distro list
phux config distro current
phux config distro use starter
phux config distro use crew
phux config distro off
phux config reload
```

Names resolve through `PHUX_DISTROS_DIR`, the XDG data directory and the build's
source checkout. Use a full layer path when the installed binary doesn't know
your checkout. Switching uses the managed distro slot, leaving personal
overrides and unrelated layers alone. It validates before writing. `off`
removes that slot, not your preferences. It does not kill panes, reset tasks,
erase the Crew home or rewrite an active agent's instructions. To change the
lead's behavior, start a new conversation in the chosen distro's home.

Theme/palette changes need `config reload`. Sidebar changes need reattach;
server-side hooks still follow normal server-start timing. Crew does not
restart or upgrade your server for you. Personal config remains higher
precedence, including plugin opt-outs, so a personal `plugins = [...]` can
intentionally suppress bundled plugins.

## See what's real

```sh
python3 distros/crew/crew.py status          # concise fleet pulse
python3 distros/crew/crew.py status --json   # native documents, availability explicit
python3 distros/crew/crew.py board           # refresh every five seconds; Ctrl-C exits only the board
phux mission list --json
phux agent list --json
```

The board never sends input, changes focus or answers permissions. Mission
listing can start phux's existing work-store keeper on demand; Crew adds no
daemon. Unavailable is different from empty. Terminal controls in names and
titles are escaped. `done_unchecked` is review pending, never accepted work.

Supervision uses native lifecycle transitions and mail. **Crew does not install
a cross-harness turn-end wake hook or promise unattended AFK supervision.**
Where the harness integration cannot wake the lead, it uses bounded foreground
checkpoints or reports truthfully that workers continue without supervision.
No hidden auto-approve, auto-merge, provider calls, telemetry or default paid
model. Build/test commands belong in the harness shell; visible panes are for
the actual agents and long-lived interactive work.

## Develop

```sh
python3 -B -m unittest discover -s distros/crew -p 'test_*.py' -v
phux config check distros/crew/crew.toml
phux plugin validate distros/crew
```

The launcher tests use fake agents and fake phux replies; no credentials or
production server required. They also run in `just guards`. Runtime config
tests verify the real layered bundle and plugin manifests.
