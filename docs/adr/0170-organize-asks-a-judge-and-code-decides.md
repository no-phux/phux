---
audience: contributors
stability: stable
last-reviewed: 2026-10-15
---

# 0170 — Organize asks a judge; code decides

**TL;DR.** `phux organize` (and the TUI's `organize` action and the
`phux_organize` MCP tool) moves panes into the sessions they belong in, in
one step. The client gathers metadata, asks a judge where each pane goes
(TypeSafe's Jev when a key is available, repository matching otherwise),
and applies only verdicts that pass rules in code: a confidence bar, and a
pane already in its repository's session never leaves it. Edits are the
typed organization commands; a local journal makes the run undoable. Screen
contents are never sent.

Status: Accepted
Date: 2026-10-15

## Context

[ADR-0160](./0160-sessions-own-resources-and-window-is-retired.md) made
sessions typed records with an Intake for unorganized work, and
[ADR-0161](./0161-organization-is-typed-commands-behind-a-feature-bit.md)
gave clients `SESSION_CREATE`, `RESOURCE_MOVE_MANY`, and `SESSION_RENAME`.
[ADR-0168](./0168-project-identity-is-a-session-attribute.md) gave sessions
a repository identity. Nothing used them together: a person sorted panes by
hand with `phux mv`, one at a time. Where a pane belongs is mostly a
repository fact, but not always: a pane titled `tail beta prod logs` in
`/tmp` belongs with `beta` by meaning, not by path.

## Decision

1. **Client-side, one path.** `phux_client::organize` gathers facts,
   plans, applies, and undoes; the CLI, the MCP adapter, and the TUI call
   it. The server learns nothing new: it sees ordinary organization
   commands from the client's actor. The pure planner lives in
   `phux_client_core::organize`.
2. **Evidence is metadata.** Per pane: its label, name or title, working
   directory, agent provider and state, run status, and the repository its
   directory is in (read from files, ADR-0168, only when the server shares
   the client's filesystem). Per session: its name, repository attributes,
   and its other members' summaries. A question about a pane never lists
   that pane as evidence for its own session. Terminal contents are never
   read for this. Titles can hold command lines, so the user-info of any
   URL in the evidence (`postgres://user:pass@`) is replaced first.
3. **Candidates are bounded.** Destinations are every session but Intake,
   plus one proposed session per repository an in-scope pane works in that
   no session carries. A numbered (generated-name) session whose panes all
   share one repository is adopted as that repository's home and renamed
   to it, rather than creating a twin. No command sets attributes on an
   existing session, so a later run recognizes the adoption by name: a
   session with no repository attribute, named after a repository one of
   its panes works in, is that repository's home. Option texts are made
   distinct, so an answer names exactly one destination.
4. **The judge only chooses.** Jev answers one Choice question per pane
   over the candidates plus "leave where it is", with a probability and a
   confidence. Without a key, with `--offline`, or with
   `organize.offline`, the repository heuristic answers instead; when Jev
   fails, the heuristic decides and the result says why.
5. **Code gates.** A verdict below the confidence bar (default 0.6) moves
   nothing and is reported as unsure. A pane already in a session carrying
   its own repository is never moved by any verdict. New sessions are
   created only when a move lands in them.
6. **Apply and undo.** The records are read again first, and a pane no
   longer where the plan saw it is skipped. Then creates (carrying the
   repository's full identity), one atomic `RESOURCE_MOVE_MANY` per
   destination, and renames. Each step reports its own refusal. The run
   writes `<state_dir>/organize/last.json` with the server's authority id;
   undo refuses another server's record, renames back, moves back panes
   still where the run put them, and deletes created sessions only while
   empty (`SESSION_DELETE` refuses otherwise). Anything changed since is
   skipped and reported, not failed, and the record is cleared once nothing
   failed.
7. **Keys stay out of argv, config, and errors.** The key comes from
   `TYPESAFE_API_KEY` or the first line `organize.jev-key-command` prints
   (no whitespace or control characters). The request goes through
   `curl -q` (no `.curlrc`) with the key and body on stdin, every config
   value escaped; an error never echoes the reply body, and the key is
   redacted from any error text.
8. **Surfaces.** The judge, the transport, the journal, and the one
   apply/undo implementation (`Sequencer`, sans-IO,
   [ADR-0171](./0171-one-transport-phux-client-rides-the-runtime.md)) live
   in `phux_client_runtime::organize`, beside repository discovery, so
   every client shares them. The CLI applies by default (`--dry-run`
   previews); the MCP tool previews by default (`apply: true` applies),
   since an agent should show a plan before moving a person's work. The TUI
   runs on its own control connection off the input loop and toasts the
   result. The desktop plans on a background thread and drives the
   `Sequencer` over the runtime's asynchronous organization lane;
   `phux_client::organize` drives it over request/reply, one request per
   submission. Both write the same journal. Jev's whole run has one time
   budget (60 seconds, 8 in interactive clients) before repository matching
   decides.

## Why

Repository identity places most panes with certainty, and the residue is a
semantic call a typed, calibrated judge answers cheaply: one request per 40
panes, sub-second. Keeping the decision in code (candidates, bar, veto,
journal) means a wrong or overconfident judgment is bounded to a
reversible membership move, never a kill, and never pulls work out of the
session that provably owns it. Running on the client keeps the server free
of network calls and credentials and keeps one path for every surface.

## Tradeoffs

- Without a key, organize is only as good as repository matching; panes
  outside git with no telling title stay put.
- Pane metadata (titles, paths, repository URLs) leaves the machine when
  Jev judges. `--offline` and `organize.offline` stop that.
- The journal is per client profile: a second run (from any surface on
  this profile) replaces the first's, and undo from another machine is not
  possible.
- Jev is used whenever a key is available; `organize.offline` is the
  opt-out, not an opt-in.

## Alternatives

**Server-side organize.** It would put a network client and an API key in
the daemon, and every surface already speaks the typed commands.

**Let an LLM write the plan.** Free-form output needs parsing and
validation and can name sessions or panes that do not exist; a Choice over
known candidates cannot.

**Heuristic only.** Deterministic and offline, which it remains as the
fallback, but it cannot place a pane whose only signal is its title.

**A new HTTP dependency.** `curl` is present on every supported platform
and keeps TLS and HTTP out of the binary; the cost is a subprocess per
request.
