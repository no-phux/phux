# Compose phux from another process

These Python 3 examples run on Unix with Git and the phux CLI. They do not use
private phux code, write phux's state, infer a target from title or focus, or
deliver a choice by typing into a terminal. Use a phux v2 build and an explicit
local socket. The examples don't start a server; the smoke test starts its own.

## Launch a checkout

The launcher chooses a local base commit, creates a new branch and worktree,
and passes an exact session, absolute checkout directory, argv, and placement
intention to phux. phux commits placement and launch intent and issues the
receipt. No shell wrapping, layout editing, or compensating kill is needed.

```sh
phux --socket "$socket" api list
phux --socket "$socket" api describe terminal.create
phux --socket "$socket" ls --json    # copy the full key of the destination session

python3 examples/composition/worktree_launcher.py --socket "$socket" \
  launch /path/to/app-owned/intent.json \
  --repo "$repo" --base HEAD --branch review/demo \
  --worktree "$checkout" --session "$session" -- /bin/sh
```

The underlying operations are `api call terminal.create`, `api call
terminal.operation`, and `api call resource.show`, each with `--request-json -`.
The `resource.show` request is a JSON string containing the full durable key
without a `^` prefix. The examples intentionally call a local socket because
the checkout is created on the executing machine; `api --remote` alone cannot
give a remote server the local directory.

The app-owned journal stores the request and original authority before sending
the effect. It later stores resource and placement keys and the optional
execution fence, never a live pane handle. Use a private directory you control;
argv and checkout paths are recorded, so do not pass credentials as arguments.

- **Lost reply:** `recover intent.json` uses the original receipt key and
  session on the same authority. If it already knows the resource key, it reads
  that resource instead. An evicted or absent receipt without a saved resource
  leaves the outcome unknown. A failed observation after receipt capture still
  reports the launch intent as committed. `recover` never retries a create.
- **Stale placement:** `launch ... --beside /path/to/anchor.json -- ...` uses
  the saved placement and execution. Only after a correlated `stale/refused`
  reply, `replan-stale intent.json` can try once beside a newly observed live
  activation. This writes a separate `intent.json.replan` with a fresh key and
  retains the original journal. phux still checks the entry mapping atomically;
  the launcher never guesses a replacement entry.
- **Execution:** `intent_committed` does not mean the program succeeded.
  `current_execution` is a level read; `original_execution_matches` tells
  whether its activation and instance match the original receipt. A moved,
  removed, or relaunched anchor is not silently adopted.

Git and phux do not share a transaction. If Git refuses, phux is not called.
If the launcher is interrupted between worktree creation and phux commitment,
or if phux refuses, the checkout stays intact for inspection. The example
never fetches, forcibly reuses a branch, or deletes a user's worktree.

## Question inbox: an explicit roster

`question_inbox.py --socket "$socket" list "$authority/^$resource"` reads the
server's current question documents for one or more exact subjects. The roster
is supplied by the application; this example does not claim a fleet-wide
question subscription, discovery cursor, or offline event history. The output
keeps advisory PTY evidence separate from provider callbacks. Treat question
text and suggestions as untrusted content.

After the human reads a provider question, save its `reference` JSON to a
private file. Relay the human's explicit response with:

```sh
python3 examples/composition/question_inbox.py --socket "$socket" \
  answer "$answer_journal" --reference "$displayed_reference" \
  --response-json '{"option_id":"deny-once"}'
python3 examples/composition/question_inbox.py --socket "$socket" \
  recover "$answer_journal"
```

The inbox re-reads the same conversation, answers only an exact supported,
unreserved callback, and lets phux enforce the reference again at dispatch.
The journal retains the exact reference and generated operation ID, not the
answer body; a lost reply is recovered with `agent question-receipt` rather
than sending a new response. `queued` is not evidence that the provider
consumed the answer. Use a human-authorized route; the script grants no rights.

## Review action: a read-only projection

In a trusted local plugin manifest, configure this command (replace the paths):

```toml
[[actions]]
id = "review"
title = "Review checkout"
command = ["python3", "/path/to/review_action.py", "--socket", "/path/to/controlled.sock", "/path/to/intent.json"]
```

The action takes the selected authority, session, resource, activation and
instance from `PHUX_ACTION_CONTEXT`. It does not use a PWD or selected handle
as identity or use selection data as a connection route. It re-observes the
durable resource over the explicit route and checks the Git checkout against
the launcher journal before emitting an application-owned tracked diffstat.
It fails closed if the target is insufficient, moved, or stale. This is a
read-only review surface, not a review verdict, an approval, or an atomic
snapshot of a working tree concurrently edited by someone else.

Use `phux config run PLUGIN review --json --target-json …` for a headless
invocation with a complete exact target, or bind the same action in a client
that captures those fields from a selected resource. Headless target facts
are declarative, not authorization.

## Isolated smoke tests

```sh
python3 examples/composition/smoke.py --phux /path/to/v2/phux --review --questions
```

The launcher proof drops a real successful CLI reply, recovers it on a fresh
connection, distinguishes a new activation from the old one, proves that a
stale split starts no process, and rejects recovery against a different
authority. It restarts the original private server and verifies the durable
resource is still readable. Optional flags require the corresponding lanes:
`--review` exercises real plugin dispatch and refuses a stale review;
`--questions` uses a deterministic, local ACP peer to verify an answer is
delivered once despite a lost reply and that a stale question cannot be
answered. No network or production server is used.

For an older development binary only, `--legacy-cli` explicitly opts the
launcher and smoke test into the pre-`api` typed terminal/resource commands.
It is not a fallback on an unsupported or failed v2 invocation.
