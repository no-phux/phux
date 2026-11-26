# Rune upstream follower

An OpenCode agent whose only job is keeping [Phux Rune](../README.md)
on the newest [Rune](https://github.com/anomalyco/rune) `dev`: bump the pin,
adapt to API and behavior changes, adopt new upstream features, prove it, and
merge it. **Every tick ships one contained, evidence-based visual or UX
improvement**, with each bump and on quiet runs too. It runs twice a day in
its own phux session, where anyone attached can watch and steer it.

| Piece | What it is |
| --- | --- |
| [`.opencode/agents/rune-upstream.md`](../../../.opencode/agents/rune-upstream.md) | The agent: its mission and rules, loaded from the worktree it runs in. It may run anything except the few commands that would move `main` around a PR |
| [`tick.sh`](./tick.sh) | One scheduled tick: heal the session and worktrees, start a fresh run unless one is busy |
| [`install.sh`](./install.sh) | The home directory and a LaunchAgent that runs `tick.sh` |

## Install

On the Mac that hosts it, from a checkout of `main`:

```sh
clients/rune/upstream/install.sh              # ticks at 07:00 and 19:00
clients/rune/upstream/install.sh --hours "6 18"
launchctl kickstart "gui/$(id -u)/sh.phux.rune-upstream"   # tick now
clients/rune/upstream/install.sh --uninstall
```

The home defaults to `rune-upstream` beside the primary checkout:

| Path | Holds |
| --- | --- |
| `phux/` | The worktree a run starts in and loads the agent from, on a clean, detached `origin/main`. Nothing is edited here |
| `worktrees/` | One linked worktree per change (`rune-upstream-abc1234`, `rune-release-VERSION`), removed once its PR merges |
| `opencode.jsonc` | Makes `rune-upstream` the default agent for OpenCode under the home only |
| `STEERING.md` | Standing orders, read first on every run |
| `PHONE.json` | Optional private phone-delivery configuration; never committed. An installation order in steering enables the delivery phase |
| `JOURNAL.md` | The agent's memory between runs, and the first place a `BLOCKED:` line shows up |
| `rune.git` | A mirror of upstream for reading ranges |
| `scratch/` | Snapshots and logs a run makes. Each tick deletes entries untouched for `RUNE_UPSTREAM_SCRATCH_KEEP_HOURS` (24) that no process holds open, except `RUNE_UPSTREAM_SCRATCH_KEEP` (`phone-*`) |
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

An attached run resumed to apply new steering is the current tick's restart,
not a reason to spawn a second live agent. Keep the twice-daily schedule and
the existing busy-run guard.

## Visual/UX work on every tick

The follower rotates eight review categories: alignment/spacing; hierarchy/
readability; density/responsive layout; navigation/discoverability; input/
touch ergonomics; feedback/empty/error/loading; motion/responsiveness; and
accessibility/contrast. Pick the strongest evidenced opportunity, recording
why an urgent finding takes priority over the next category. Quiet ticks
take a contained [MOBILE.md](../MOBILE.md) piece or improve
an existing flow. A current pin is not a no-work exemption.

[Visual review](../VISUAL_REVIEW.md) defines the maintained
capture command, realistic scenario gallery, before/after judgment template
and independent fresh-context review:

```sh
bash clients/rune/scripts/visual-review.sh /absolute/home/scratch/TICK/after
```

Run it from the change worktree; use separate before/after output directories
outside Git. Its real Rune Runtime/GPU exporters cover Settings, phone
navigation, resources and phone Inbox, producing PNGs, trees and a manifest.
Review populated core screens plus the selected flow's constrained,
interactive, light/dark and 2× cases. Capture the selected flow before editing,
state what became easier and fix regressions before merge; unchanged pixels
or a passing exporter are not a visual judgment. Do not retry captures into
looking good or add a new AI dependency/scoring framework. Existing input
ownership, literal pane arrangement and the good command palette stay intact.

Bumps move all ten upstream manifest entries together: four direct Rune
dependencies and six crates.io patches, including `sctk-adwaita` for Wayland
frame corners, not the removed `rune-mac` entry. Recheck upstream's patch set.
All ten pins, including `winit`, now use the same immutable upstream revision.
Upstream [#1454](https://github.com/anomalyco/rune/pull/1454) supplies `ImeSync`
to suppress unchanged cursor-area calls; the temporary fork and its six-helper
gate are retired. Validate this adoption with the two actual upstream
`ImeSync` tests listed in [DEVELOP.md](../DEVELOP.md), run from
the pinned upstream workspace, not a dependency-test wrapper. Client Rust/IME,
real Wayland compositor, packaged/native qualification and release gates
remain mandatory.
The release policy follows the current patch-release train in
[Releasing](../../../docs/releasing.md#phux-rune), not the historic alpha sequence.
Visual review supplements all existing check, packaged/native qualification
and device-delivery gates; it cannot replace them. A truly blocked tick
reports the evidence and exact remedy rather than calling maintenance shipped.

## Ready is not CI qualification

For feature **and release** PRs, use **Ready → genuine CI → auto-merge**.
After local validation, mark Ready, then require successful normal CI on the
current head, its plan/selected lanes, and `ci`/`commitlint` before enabling
auto-squash. `gh pr checks --watch` can return only old draft-skipped checks
before the new run registers. GitHub can count that skipped aggregate as passing
and merge early; neither `SKIPPED` nor `MERGED` proves the normal run passed.
If a PR nevertheless merges prematurely, record that gap and retain exact-main
CI/full-native/tag/device gates. This job-level ordering does not fix the
repository-wide draft-check race.

## Steering

```sh
phux attach rune-upstream          # watch the run, type to steer it
phux tag add '#rune-upstream-run' hold      # pause: ticks skip while any pane has #hold
phux tag rm '#rune-upstream-run' hold       # resume
```

Typing into the live run steers that run. An order that should last, such as
`release: auto`, goes in `STEERING.md`; tell the run and it writes it there.

## Scheduled iPhone updates

Phone installation is a separate opt-in from desktop releases. With an
explicit installation order in `STEERING.md`, the agent reconciles the phone
after merged Rune changes **and on quiet ticks with an update pending**.
It uses the existing signed ad-hoc distribution path, not TestFlight.
[Native delivery](../ios/README.md#scheduled-phone-updates)
documents the private `PHONE.json` schema, source/build identity and commands.

The agent attempts an in-place install through the already paired device
and independently checks the installed marketing/build versions. It never
uninstalls, resets pairing, clears credentials or launches the user's app.
A signed OTA page remains a fallback when direct device access is unavailable;
opening its install link and any Apple prompts still require on-phone approval.
Locked signing access or an unavailable phone is reported as pending, not success.
The existing production Phux binary/server and shared Tailscale hosting are
not replaced or reset to deliver an update.

Every tick's private closing summary includes the **same existing permanent
tailnet `/scheduled/` install page**, even after a verified device install.
Verify it answers and discovers the latest qualified, signed, fully published
phone build through the existing host; if publication is blocked, retain the
last verified qualified build and say exactly what is pending. Immutable
per-build URLs remain internal evidence/fallbacks, not replacement bookmarks.
Do not change hosting or start a second daemon. Never put private tailnet
links or signing/device configuration into a public PR. Page availability,
signed publication and independently queried installation are separate facts.
See [One permanent download page](../ios/DELIVERY.md#one-permanent-download-page).

## Steering from another tailnet

The session is ordinary phux, so anyone who can reach this server can attach
to it. A trusted person on another tailnet either gets this machine shared
into their tailnet, or a [relay](../../../docs/remote.md#relays) both can reach,
and then a pairing link from `phux pair`. **A paired device controls the
whole server**, every session on it ([Who can do what](../../../docs/remote.md#who-can-do-what)).
To give a co-steerer this job and nothing else, run the follower on its own
server: `install.sh --profile rune` runs every tick against the `rune` profile,
and pairing that profile's server shares nothing else.
