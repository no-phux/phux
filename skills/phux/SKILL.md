---
name: phux
description: Drive phux from an agent. Read a pane, act on it, wait for an observed transition, verify. Covers the am-I-inside-phux check, the selector grammar, the level-versus-edge rule that decides whether a supervision loop is correct or broken, exit codes, the JSON error contract, and the safety rules for driving a terminal a human may also be using.
---

# Driving phux from an agent

phux is a terminal multiplexer with a headless control plane. Every verb here
works without a TTY, so an agent can create panes, read what is on them, type
into them, and wait for something to happen.

This file is compiled into the `phux` binary. `phux --skill` prints the copy that
belongs to the binary you are actually running, so it cannot describe a verb
this build does not have. If you obtained this text any other way, run
`phux --skill` and trust that instead. `phux skill` is an equivalent command.
Use `--skill=quick`, `--skill=agent`, or `--skill=terminal` for focused output;
`--skill=full` is the default and includes everything.

A current state is a level read; completion requires an observed transition.
Always bound waits. Exit 124 is an observation timeout; `phux run` reserves 125
for its own timeout because it otherwise mirrors the child process status.

Deeper reference: `phux help <verb>` for any verb below. Every claim in this
file is about the binary that printed it.

---

<!-- phux-skill-region: quick -->
## First: are you inside phux right now?

phux injects two environment variables into every pane it spawns.

| Variable | Meaning |
|---|---|
| `PHUX_TERMINAL_ID` | the numeric id of the pane **you** are running in |
| `PHUX_SOCKET` | the UDS path of the server that owns that pane |

```sh
if [ -n "$PHUX_TERMINAL_ID" ]; then
  echo "inside phux; this pane is @$PHUX_TERMINAL_ID"
else
  echo "not inside a phux pane"
fi
```

Do this before you drive anything. Two failures depend on it:

- **Do not send input to your own pane.** `phux send-keys @$PHUX_TERMINAL_ID`
  types into the terminal you are reading from. If you are an agent under a
  supervisor, that is you prompting yourself, and it usually deadlocks.
- **Do not read your own pane as if it were the subject.** A snapshot of
  `@$PHUX_TERMINAL_ID` shows your own transcript, which will happily contain
  whatever text you were waiting to see.

`PHUX_SOCKET` is already the default every verb dials. Pass `--socket PATH`
only to reach a different server.

---

## The loop

Read, act, wait, verify. Never act and assume.

```sh
phux snapshot --json @7                       # read
phux send-keys @7 "cargo test" Enter          # act
phux wait --until "test result:" --timeout 300 @7   # wait
phux snapshot --json --tail 200 --unwrap @7   # verify
```

When you only want an exit code, `phux run` collapses act-and-wait into one
call and mirrors the child's status, so it composes like a shell:

```sh
phux run --json --timeout 300 @7 "cargo test" && echo passed
```

The contrast: `run` is "I want the exit code". `send-keys` plus `wait` is "I am
driving an interactive or long-lived program that does not exit".

When the input is a block of text — a heredoc body, indented code for a REPL,
multiline SQL — use `paste`, then submit explicitly. `send-keys` would type it
character by character and let the program's auto-indent mangle it.

```sh
phux paste @7 "$(cat snippet.py)"
phux send-keys @7 Enter
```

Flags belong to the verb and, on `run` / `wait` / `send-keys`, must come
**before** `TARGET` — the trailing arguments are the command or the keys, and a
flag placed after them is swallowed into the payload.

---

## Targets: the selector grammar

`TARGET` is resolved on the client side; the server never sees a selector.

| Form | Resolves to |
|---|---|
| `@N` | one pane, by opaque local id. The form you should prefer |
| `host/@N` | one pane on a federation satellite |
| `.` | the focused session |
| `name` | every pane of the session called `name` |
| `name:W` | window `W` (index or window name) of that session |
| `name:W.P` | pane `P` of window `W` |
| `#tag` | every pane carrying L3 tag `tag` (see `phux tag`) |
| `%name` | reserved; no shipped verb resolves it yet, so it fails closed |
| `=` | refused: it means the attached TUI's focus history, which a headless caller does not have |

Get ids from the JSON of the verb that created the pane (`phux new --json`,
`phux spawn --json`, `phux launch --json`) or from `phux ls --json`. Ids are
stable for the life of the pane and are never reused while it lives.

Whole-session and `#tag` selectors are **set-valued**: `phux kill build` kills
every pane in the session, and `phux tag add #web ci` tags every match. Verbs
that need exactly one pane pick a representative from a set, which is fine for
a read and wrong for a write. Address writes with `@N`.

`%name` is parser-reserved for a proposed agent-name addressing contract, but
no shipped verb resolves it. It fails **closed** as a selector miss rather
than landing on the wrong pane. Use `@N`, especially for writes and destructive
operations.

**Sigil hygiene.** `%` is shell-safe unquoted in `sh`, `bash`, and `zsh`, which
is why it was chosen. It is *not* safe everywhere else: `%` is a format
specifier to `printf`, and `%name` opens a pattern rule in a Makefile. Quote or
escape it when a selector is interpolated into anything but a shell command
line. `#tag` is the mirror image: safe in a Makefile, a comment in an
interactive shell, so quote it there (`phux kill '#build'`).

---

<!-- phux-skill-region: agent -->
## Level versus edge: the rule that decides if your loop is correct

This is the single most important paragraph in this file.

A **level read** answers "what is true right now". A **edge read** answers
"what changed". They are not interchangeable, and a completion gate needs the
edge.

`idle` is the detector's fail-safe fallthrough. No shipped rule asserts it
positively — it is what phux reports when nothing contradicts it. So `idle` is
equally true of:

- an agent that finished its turn,
- an agent that crashed,
- a pane that never ran an agent at all,
- a pane sitting in `less`,
- a TUI caught half-painted between frames.

Therefore:

- **`phux agent show` is the level read.** It tells you the current state and
  asserts only the absence of contrary evidence. It is never proof of life and
  never proof of completion.
- **`phux agent wait` is the edge read.** It is satisfied *only* by an observed
  transition into a `--until` state. It subscribes before reading its baseline,
  so no transition can slip through the gap, and it re-polls to recover an edge
  a dropped notification never delivered.

The deliberate consequence: **a pane already resting in the state you asked for
times out.** That is not a bug, and it is the case an LLM gets wrong. Exit 124
means "no transition was observed", which is a *different statement* from "the
agent is still working".

```sh
phux agent send-keys --expect-agent reviewer @7 "review the diff" Enter
phux agent wait --until idle --until blocked --timeout 900 --json @7
```

Read the answer, do not assume it:

| Exit | Meaning | What to do |
|---|---|---|
| 0 | a transition into one of the `--until` states was observed | proceed; `edge` in the JSON says `from`, `to`, and whether it arrived by push or poll |
| 124 | no transition inside the timeout | read `baseline` in the JSON. If it already equalled your target, the agent was resting the whole time — decide with `phux agent show` plus a `snapshot`, not by waiting again |
| 1 | the agent **departed**: the record was deleted, or its state was withdrawn to `unknown` because the occupant died or changed | this is never completion |
| 2 | the pane declares no agent record at all, or an unspellable `--until` word | there is no lifecycle here to wait on |

`--until` defaults to `idle`, `blocked`, `done` — the three ways a turn ends.
`unknown` is not spellable: it is departure, not a state.

`--timeout` is **unbounded when omitted**. Always pass one.

Writing the same loop against `phux wait` instead is the common mistake:
`phux wait --idle 750 @7` tells you pixels stopped changing, which a crashed
pane also satisfies. Use `phux wait` for screen conditions in a program you
drove yourself; use `phux agent wait` to supervise an agent.

### When the pane runs the shipped Claude shim

`phux agent install-claude` makes plain `claude` launch inside phux and declare
its identity. Installing a **newer phux binary does not rewrite an installed
shim**. An older shim rewrote the agent record on every lifecycle hook, which
published a spurious departure edge at the end of every turn — so `agent wait`
exits 1 on a healthy agent, once per turn. If you see that, the fix is:

```sh
phux doctor                  # reports a stale shim
phux agent install-claude    # rewrites it; a running claude picks it up next session
```

`phux agent uninstall-claude` removes the shim and its shell activation. Both
are the human's call: they edit a shell rc file. Report the diagnosis, do not
run the fix unasked.

---

<!-- phux-skill-region: terminal -->
## Screen reads

`phux snapshot` is the floor: it walks the server's own grid, so it neither
attaches nor resizes the pane and is safe to poll against a pane a human is
using.

```sh
phux snapshot --json @7                       # viewport
phux snapshot --json --scrollback 500 @7      # plus history
phux snapshot --json --tail 200 --unwrap @7   # last 200 logical lines
phux snapshot --json --cells @7               # per-cell styles + OSC-133 marks
```

- `--unwrap` joins soft-wrapped rows into lines **as written** rather than as
  painted. Use it whenever you are going to regex the result; a path or a test
  name that straddles the right edge is otherwise two strings.
- `--tail N` counts rendered rows, and the viewport is a floor — you can get
  more rows than you asked for, never fewer. `truncated` reports drops.
- `--cells` cannot combine with `--unwrap`: cell coordinates do not survive
  the join.
- `--rendered` is the exception to side-effect-freedom: it composites the
  multi-pane view the way a human's screen shows it, which drives the client
  render path and therefore attaches. Do not poll it.

`phux wait` polls that same read:

```sh
phux wait --until "BUILD SUCCESSFUL" --timeout 300 build
phux wait --regex "test result: (ok|FAILED)" --output-only --timeout 300 build
phux wait --idle 750 --timeout 60 repl
```

Matching is against lines **as written** — wrapped rows are joined first, so a
match that straddles a wrap is found rather than silently never matching.

`--until` matches any line, **including the shell's echo of the command you
just typed**. Two defences, and you want one of them every time:

1. `--output-only` ignores lines the shell marked as typed input. It needs a
   shell with OSC-133 integration; with none, nothing is filtered and phux says
   so on stderr rather than pretending.
2. Failing that, match on text that can only appear in output. `--until "test
   result:"` is safe after `cargo test`; `--until "cargo test"` is not.

`phux watch` is the push half: one event per line, as it happens, instead of
on a poll tick. The event vocabulary is `agent_state`, `asked`, `bell`,
`command_started`, `command_finished`, `dirty`, `idle`, `pane_spawned`,
`pane_closed`, `title_changed`, `unknown`.

```sh
phux watch --json --until idle --until asked --timeout 60 @7   # bounded, exits 0 on a hit
phux watch --json --timeout 60 @7 > events.jsonl               # bounded collection
```

`--until` exits 0 on the first matching event and is repeatable (any one of
them satisfies it); an unrecognized event name is a usage error (exit 2)
raised before the watch starts, never a watch that quietly never matches.
`--timeout` exits 124 and applies with or without `--until`. Without
`--timeout` the stream runs until EOF or you kill it, so if you go that route,
bound it yourself and reap the child:

```sh
phux watch --json @7 > events.jsonl &
watcher=$!
( sleep 60; kill "$watcher" 2>/dev/null ) &
```

Watch several panes with several concurrent watchers, so one quiet pane cannot
block collection from the others. A watcher exiting is not evidence that work
finished. Verify with a read.

---

## Acting on a pane

```sh
phux send-keys @7 "echo hi" Enter        # named keys or literal text
phux send-keys @7 C-c                    # control and meta: C-c, M-x, Up
phux paste @7 'SELECT count(*) FROM users;'
git diff | phux paste @7                 # TEXT omitted reads stdin
phux run --timeout 30 --json @7 "cargo test"
phux signal @7 freeze                    # SIGSTOP: the reversible brake
phux signal @7 resume
```

- **Input is real input.** `send-keys` writes to a live PTY. It is not a
  clipboard and not an API. A literal run immediately before `Enter` is sent as
  one submission-safe paste.
- **An unknown delivery is never a retry.** `send-keys` and `paste` are
  fire-and-forget: they give you no receipt. The verbs that *do* acknowledge a
  batch (`agent prompt`, `agent answer`, `agent start`) can still end in "I
  could not prove this landed", and that outcome is **terminal** — see the
  three rules under `phux agent prompt` below. Never answer it by issuing the
  work again under a fresh operation id: the first write may have succeeded,
  and the second one runs the turn twice. Read the pane and decide from what
  you see.
- `phux ask` reports that an agent is blocked on a human answer. It raises the
  advisory event the TUI and dashboards consume; it does not move anyone's
  focus.

<!-- phux-skill-region: agent -->
### Answering a question an agent asked

An `asked` event carries the question **and** the suggestions the asking agent
published, so reply with one of its own answers rather than a blind keystroke:

```sh
phux agent answer --id deploy --choice 1 --json @7      # the 1st published suggestion, verbatim
phux agent answer --id deploy --text "no" --json @7     # refused unless "no" was published
phux agent answer --id deploy --text "later" --allow-unlisted @7
```

`--id` is required and must still be the question the pane is asking. That is
the whole point: answering a question the agent already moved past would type
into whatever is on screen now. A stale id, an unidentified ask, and a pane
that is not asking are all refusals with nothing written. The answer rides one
acknowledged, idempotent input batch — a trusted paste plus Enter, confirmed as
a single operation.

### Giving an agent a turn's work: `phux agent prompt`

This is the verb for handing an agent prose to act on. Use it instead of
`send-keys` whenever the payload is a task rather than a keystroke.

```sh
phux agent prompt --expect-agent reviewer @7 "review the diff and report"
phux agent prompt @7 "run the tests" --wait --until idle --until blocked --timeout 900 --json
```

The prompt text and Enter ride **one acknowledged, idempotent operation** under
a client-generated operation id. That buys the one thing fire-and-forget input
cannot give: a caller that loses the answer can ask again under the same id
without risking a second turn. `phux` never generates a new id for a retry.

Three rules, in the order they will bite you:

1. **`delivery: "unknown"` (exit 1) is terminal. Do not resend.** Some, all, or
   none of the bytes reached the pane, and a batch reported unknown can still
   complete a moment later. Resending under a new id runs the turn twice;
   resending under the same id replays this same answer. **Read the pane**
   (`phux agent explain`, `phux snapshot`) and decide from what you see.
2. **A success is a kernel-queue receipt, not a consumption receipt.** `OK`
   means every byte was accepted into the pane's tty input queue. It does not
   mean the agent processed them: a TUI that flushes its input queue — which
   every TUI does when it shells out and comes back — discards an acknowledged
   batch silently. When that happens the honest signal is `--wait` timing out
   at 124, not a shorter guess.
3. **`--wait` is the same edge gate as `phux agent wait`.** Only a transition
   *observed after the write* satisfies it; the pre-submit level never does.
   Exit 124 means "delivered, no transition observed", which is a different
   statement from "the prompt failed".

Refusals (exit 2, nothing written): prompt text containing a raw newline (a
pane that has not enabled bracketed paste turns each newline into a separate
submission, and no client can observe that mode — send one line, or put the
long form in a file and prompt with the path); text over 4096 bytes; a
satellite target; a server too old to acknowledge input; a pane whose agent
record is absent or names someone else. phux never splits one prompt across
two operations and never silently downgrades to fire-and-forget delivery.

Exit 1 with `input_busy` means the server's **single** acknowledged input lane
stayed busy — nothing was written, so re-running is safe. There is one such
lane per server, so **do not prompt a fleet in parallel**: serialize it, or all
but one caller collides.

### Writing keystrokes into a pane that hosts an agent

`phux agent send-keys` is the identity-checked sibling of plain `send-keys`,
for keystrokes rather than prose — `C-c`, an arrow, a bare `y`. It re-reads the
pane's agent record immediately before writing and refuses if the occupant
changed:

```sh
phux agent send-keys --expect-agent reviewer --expect-kind claude @7 "go" Enter
```

Every key is validated before any byte is written, so a typo in the third key
cannot leave the first two delivered — and the whole batch now rides one
acknowledged operation, so that all-or-nothing promise covers *delivery* too:
`--json` reports `delivery` and `operation_id`, and the `delivery: "unknown"`
rule above applies here verbatim. Exit 2 is a refusal with nothing written
(bad key spec, no record, wrong occupant, too many keys for one batch); exit 1
is a delivery or transport failure. Plain `phux send-keys` deliberately checks
no identity and carries no receipt — use it when a *pane* is what you mean, and
the agent form when an *agent* is.

The reserved `%name` resolver includes a withdrawn-record safety gate, but no
shipped write verb calls that resolver yet. Address the pane directly with
`@N`; identity-sensitive writes still re-read and verify the record through
`--expect-agent` and `--expect-kind`.

---

## Agent identity and state

### Starting an agent in a pane you already have

```sh
phux agent start --kind claude --target @7 --timeout 120 --json reviewer
```

`phux agent start` creates, splits, and moves nothing: it types the
integration's launch command into a pane whose child is a live shell, submits
it as one acknowledged batch, binds `NAME` to the pane, and returns when the
agent is **ready for input**. `NAME` must match the reserved agent-name grammar,
but the shipped selector surface still addresses the pane by `@N`.

Ready means the first detector publication after submit, not `state == idle` —
`idle` is the fail-safe fallthrough, so a gate on it would report ready for a
pane where nothing launched. `--json` reports which rule matched. Exit 124
means readiness was not observed inside `--timeout`, and **the command was
still typed**: read the pane before doing anything else. `--no-wait` submits
and returns without claiming readiness. A `--kind` with no detection manifest
is refused up front rather than typing and then timing out.

Compare: `phux launch` and `phux spawn` create a pane and promise nothing about
readiness; `phux agent start` promises readiness about a pane you already had.

### Reading and writing the record

```sh
phux agent list --json                 # every pane, inferred state
phux agent show --json @7              # one pane, the LEVEL read
phux agent explain --json @7           # the evidence trail behind that state
phux agent set @7 --name reviewer --kind claude --session review-fleet
phux agent report-state @7 done          # integration-hook evidence
phux agent clear @7
```

`agent list`/`show`/`explain` share one JSON shape: per pane a `terminal`,
`agent` (`id`, `label`, `kind`), `state` (`unknown` | `idle` | `working` |
`blocked` | `done`), `confidence`, `attention` (`none` | `low` | `normal` |
`high`), and `sources` — the provenance trail, sorted by descending
confidence. Read `sources` when a state surprises you: it tells you whether the
state was *declared* by an integration (`agent_record`) or *derived* from the
screen, the title, or process identity.

`phux agent set` writes an explicit record. Two rules with teeth:

- Only `--name` is required. A record with just a name is identity-only, and
  its resting `state` is the literal `unknown` — which is **not** a
  declaration, so the server keeps deriving state around it. That is usually
  what you want.
- Passing `--state` **outranks the detector for the lifetime of the record**.
  You have taken over reporting; nothing will correct you. Use it only if you
  will keep it current, and `phux agent clear` when done.

`phux agent report-state TARGET working|blocked|done` is for lifecycle hooks.
It feeds immediate evidence into an already identified pane's detector without
writing the record, so later process and screen evidence can still correct it.

`phux agent explain --file capture.json --kind claude` runs entirely offline
against a captured screen and contacts no server — the mode for debugging why
a pane's state is what it is, because it prints the text every region resolved
to. A rule scoped to a region that comes back empty can never match, and
nothing else makes that visible.

---

<!-- phux-skill-region: quick -->
## Exit codes

Uniform across the surface unless a verb had to spend the number on something
else.

| Code | Meaning |
|---|---|
| 0 | success |
| 1 | failure: no server, no such target, the verb failed |
| 2 | usage error, or the server or verb refused |
| 3 | unanswerable: the selector was resolved against a partial view of the fleet (a satellite was unreachable). Unlike 1, the target may exist — retry |
| 124 | `phux wait` / `phux agent wait` timed out |
| 125 | `phux run` gave up on `--timeout` |

`phux run` mirrors its child's exit code into `0..=255`, which is exactly why
its timeout is 125 and not 124: real commands exit 124 (GNU `timeout` does), so
`run` reserves the wrapper-failure convention for itself.

**1 versus 3 is the branch that matters.** A hub that cannot reach a satellite
still answers, with that satellite's panes simply missing. So "no such target"
and "I could not look where it lives" must not collapse into one status.
Retrying is right for 3 and wrong for 1. Verbs that already spent 3 on
something else say it in words on stderr instead; `kill`, `tag`, and
`agent show`/`explain`/`set`/`clear` spend the status.

Verbs that *enumerate* cannot spend a status on it, because a partial listing
is still a successful listing. `phux ls --json` therefore reports it
structurally: **check `unreachable`**. It is emitted even when empty, and an
empty array is the only thing that makes the inventory complete. A degraded
listing is otherwise byte-identical to a whole one, and treating one as whole
is how an orchestrator concludes a pane is gone and kills its replacement.

---

## JSON

`--json` is per verb, always after the verb. There is no `-j`.

On success the document goes to stdout. On failure **stdout stays empty** and
stderr carries exactly one JSON object:

```json
{
  "schema_version": 1,
  "error": { "code": "no_server", "message": "no server running at /run/phux.sock" },
  "remedy": "start one with `phux` (attaches, auto-starting a server) or `phux server`",
  "exit_code": 1
}
```

Branch on `error.code`, never on `message`. The vocabulary is closed. The codes
you will actually meet: `no_server`, `server_disconnected`, `transport`,
`no_such_target` (a miss against a complete view), `partial_view` (a miss
against an incomplete fleet — retry), `no_agent_record`, `agent_departed`,
`agent_mismatch`, `invalid_key_spec`, `json_serialize`. `remedy` is always
present and non-empty: it is the next command to run, in prose.

Two documented exceptions to "stdout stays empty on failure", both deliberate:
`phux wait --json` and `phux agent wait --json` write their result document to
stdout on a timeout (exit 124) so you can read `baseline`; and
`phux status --json` writes `{"running": false, ...}` when no server is up.

---

## Safety rules

1. **Every wait is bounded.** Give `phux wait`, `phux agent wait`, and
   `phux watch` a finite `--timeout`. All three run forever without one, and an
   unbounded wait inside an orchestration loop is a hang with no operator.
2. **Never take a human's focus.** Placement and layout verbs write the
   persisted tree; they do not move an attached human's viewport, and there is
   no headless verb that does. Do not simulate one by sending the TUI's own
   chords through `send-keys` — `C-a q` and `C-a Q` belong to the human's
   keyboard. Surface a blocked question with `phux ask` and tell the human to
   press them.
3. **Confirm before anything destructive.** `phux kill`, `phux signal
   terminate`/`kill`, and any interrupt that can lose work: resolve and display
   the exact selector, snapshot the state that is about to disappear, say what
   will be lost, get an affirmative answer, then issue the narrowest operation,
   then verify by re-reading.
4. **Prefer `@N` for every write.** A session name or a `#tag` is a set, and a
   set-valued write does more than you meant.
5. **`take`/`give` is not a lease you can hold.** `phux take` seizes exclusive
   input authority for the *connection* that holds it. A one-shot CLI call ends
   when the process ends, so there is no durable agent lock here. Do not model
   one.
6. **Do not invent credentials, schedules, retries, or ownership.** phux has no
   scheduler and no credential channel. Subprocess deadlines are observation
   mechanics, not a runtime.
7. **A quiet pane is not a finished pane.** Verify with a read (`snapshot`,
   `agent show`, `ls --json`), never by inferring from the absence of output or
   from a watcher exiting.

---

<!-- phux-skill-region: full -->
## The whole surface

Verbs you will use constantly are above. This is the complete inventory of the
binary, so nothing is invisible to you.

**Read**

| Verb | What it does |
|---|---|
| `phux ls` | list sessions. Does not start a server; exits non-zero if none is running |
| `phux status` | the running server: pid, uptime, protocol, clients, log paths |
| `phux perf` | the server's always-on performance telemetry: latency histograms per stage, throughput, CPU, RSS; `--watch 1` for live per-second intervals, `--json` for the raw report |
| `phux snapshot` | side-effect-free screen read, as JSON or a boxed view |
| `phux watch` | stream live pane events, one per line; `--until EVENT` and `--timeout` bound it |
| `phux wait` | block until a screen condition holds |
| `phux agent` | `list`, `show`, `explain`, `set`, `clear`, `wait`, `prompt`, `send-keys`, `answer`, `start`, `install-claude`, `uninstall-claude` |
| `phux workspace` | `inspect` a git repo and its worktrees; `save` / `restore` a session archive |
| `phux doctor` | diagnose the install: config, socket, server, plugins, shims |
| `phux logs` | where the logs live, or tail one |
| `phux --skill` | print this file (`phux skill` is equivalent) |
| `phux mcp` | run the bundled MCP stdio adapter; forwards `--skill`, `--schema`, help, and future arguments |

**Act**

| Verb | What it does |
|---|---|
| `phux new` | create a session. `--json -s NAME` creates without attaching |
| `phux spawn` | create a pane running explicit argv, optionally placed |
| `phux launch` | create a pane running a configured integration by name |
| `phux run` | run one command in a pane and mirror its exit code |
| `phux send-keys` | send named keys or literal text to a pane |
| `phux paste` | paste a block of text (bracketed when the pane asks) |
| `phux ask` | report that a pane is blocked on a human answer |
| `phux kill` | destroy a session, window, or pane |
| `phux signal` | POSIX signal to the pane's process group; `freeze`/`resume` are reversible |
| `phux take` / `phux give` | seize and release exclusive input authority on a pane |

**Shape**

| Verb | What it does |
|---|---|
| `phux insert-pane` | put an already-created pane into a layout |
| `phux move-pane` | move an existing pane beside another, across sessions too |
| `phux swap-pane` | exchange two leaves, preserving split geometry |
| `phux resize` | set a pane's grid size with no TTY |
| `phux rename` | rename a session |
| `phux tag` | `ls` / `add` / `rm` freeform L3 tags; address them with `#tag` |
| `phux detach` | detach clients from a session, from outside the UI |

**Record**

| Verb | What it does |
|---|---|
| `phux rec` | record a pane to an asciinema cast, a GIF, or an APNG |
| `phux play` | play a recording back as a live pane |

**Configure and operate** (mostly a human's job, listed so you can read it)

| Verb | What it does |
|---|---|
| `phux attach` | attach interactively. Requires a TTY — not for you |
| `phux server` | run a server in the foreground |
| `phux service` | keep a server running across logout and reboot |
| `phux config` | `init` / `path` / `check` / `show` / `plugins` / `agents` / `reload` / `run` |
| `phux plugin` | manage local plugin manifests in the config registry |
| `phux worktree` | `new` / `open` / `list` / `remove` git worktrees with sessions bound to them |
| `phux completion` | print a shell completion script |
| `phux update` | update phux to the latest release, keeping sessions alive |
| `phux upgrade` | hot-swap the running server binary in place |
| `phux host` | register the machines phux talks to: remotes and satellites |
| `phux pair` | mint a pairing token for a remote consumer |
| `phux relay` | run a standalone relay, or enroll a route with it |

---

<!-- phux-skill-region: agent -->
## A worked supervision loop

```sh
set -eu

# 0. do not drive yourself
me="${PHUX_TERMINAL_ID:-}"

# 1. create a pane and give the agent in it an addressable name
pane=$(phux spawn --json -c "$PWD" -- claude | jq -r .terminal_id)
[ "$pane" != "$me" ] || { echo "refusing to drive my own pane" >&2; exit 1; }
phux agent set "@$pane" --name reviewer --kind claude --session review-fleet

# 2. hand it a turn's work and wait on an OBSERVED TRANSITION, bounded.
#    One call, so there is no window between the write and the wait.
set +e
result=$(phux agent prompt --expect-agent reviewer "@$pane" "review the diff" \
           --wait --until idle --until blocked --timeout 900 --json)
status=$?
set -e

# 3. branch on what actually happened
case "$status" in
  0)   echo "$result" | jq -r '.edge | "\(.from) -> \(.to) via \(.via)"' ;;
  124) echo "delivered, but no transition in 900s" >&2 ;;
  1)   echo "delivery unknown or the agent departed; DO NOT resend" >&2 ;;
  2)   echo "refused, nothing written; fix the call" >&2; exit 1 ;;
esac

# 5. verify by reading, not by assuming
phux snapshot --json --tail 200 --unwrap "@$pane" > transcript.json
```

---

<!-- phux-skill-region: quick -->
## What phux is not

Not a scheduler, not a credential channel, not a durable lock service, and not
a message bus. It is explicit terminal identity plus observable state. If a
plan needs guaranteed delivery, a lease that outlives a process, or a retry
policy, that logic belongs in your orchestrator — phux will not supply it, and
pretending otherwise is how supervision loops go wrong.
