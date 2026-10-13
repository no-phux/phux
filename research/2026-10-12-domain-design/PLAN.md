---
audience: contributors, agents
stability: scratch
last-reviewed: 2026-10-12
---

# Build plan

**TL;DR.** Seven stages, each a mergeable PR series with its own gate, ordered so every stage ships working software and nothing waits on workspaces or browsers. Stage 0 writes the ADRs and spec text. Stage 1 rebuilds `phux-core` around sessions and resources with no Window. Stage 2 adds the org store and activations. Stage 3 puts typed organization on the wire. Stage 4 moves every client. Stage 5 makes actors real. Stage 6 adds workspaces; stage 7 the browser kind.

Each stage lists what lands, what it deletes, and the test that proves it. `just ci-full` is the bar for every PR. No stage leaves two authoritative models running: compatibility layers translate into the new model, never alongside it.

## Stage 0: decisions on paper

| Deliverable | Content |
|---|---|
| ADR: resources, sessions, workspaces | Nouns, D01, D03, D04, D06; retires Window; supersedes ADR-0105's keep-empty and the L2 claim that grouping is client convention. |
| ADR: typed organization surface | D08; the `Organization` capability; translation of intercepted keys; partially supersedes the session-key sections of L3. |
| ADR: persistent records and activations | D02, D07; org store format and write discipline; startup reconciliation by process identity; exit policy; removes respawn-in-place. |
| ADR: actors and delegated agent grants | D09; `Organize` and `Terminate` scope verbs; human lease precedence; lease epochs. |
| `docs/CONCEPTS.md` | Rewritten around the model, marked as landing in stages. |

Gate: `just docs-check`; the ADRs reviewed. Specs in `docs/spec/` change with the code that implements them, in stage 3.

## Stage 1: core model

`phux-core` only, plus the minimal server edits to compile.

1. Add `SessionKey`, `ResourceKey`, `ArrangementKey`, `Revision`, `ActivationId` (CONTRACTS §01). Keys are 16 random bytes; core takes them from the caller, it does not generate randomness.
2. Replace `Session{windows, active, keep_empty}` with `Session{key, name, lifetime, members, arrangement, attrs, revision}`.
3. Delete `Window`. Move `LayoutNode` to `arrangement.rs`, generic over its leaf. Add `Arrangement` and its validation (session arrangements reference members only; entry IDs unique; ratios valid).
4. `Resource` gains `key`, `session`, `on_exit`, `activation`; loses `window`.
5. Add `apply(&mut Org, Command, &Actor, now) -> Result<Applied, Refusal>` where `Applied` lists the transition events and the effects (launch, terminate) for the server to run. Every command in CONTRACTS §04 that touches sessions, resources, or arrangements is implemented here.
6. Server: the registry wrapper keeps its current behavior through the new types. Today's single window becomes the session's arrangement with one group. The wire is unchanged: `GetState` still reports one window per session, synthesized from the arrangement.

Deletes: `window.rs`, the window steps of the reap cascade.

Proof: property tests over random command sequences. Invariants: every resource is in exactly one session's members; arrangements reference only members; revisions strictly increase; ephemeral sessions are never empty after a commit; no transition ever both removes a resource and leaves its activation running. Existing server and e2e tests pass unchanged.

## Stage 2: store, activations, recovery

1. `OrgStore` trait plus the JSON implementation (CONTRACTS §08), with crash-safety tests that cut the write at each step.
2. Mint `AuthorityId` on first start; import the live registry and any `phux.tui.layout/v1` documents (MIGRATION §07).
3. Terminal executor records `ProcIdentity` (pid, pgid, start time) and reports activation transitions.
4. Startup reconciliation: rebind by handover, `Lost` by matching start time, `Unobserved` otherwise. Never relaunch.
5. Exit policy replaces respawn-in-place and ADR-0124's retention flag. Server lifetime becomes "no live activations and no clients".
6. Receipts for `OperationId`.

Deletes: respawn-in-place, the zero-session exit, the keep-empty reap step.

Proof: kill the server with SIGKILL mid-session and restart it: records come back; dead shells show `Unobserved`; a `nohup` child shows `Lost`; nothing relaunches. Restart again: same result, no double-counting. Store-write failure leaves memory unchanged.

## Stage 3: wire

1. `ServerFeatureExt::Organization`; the new `Command` variants, `Refusal`, and events (CONTRACTS §04, §05, §07); `ActorRef` gains principal fields.
2. Scope verbs `Organize` and `Terminate`; classify every command; `Group` selectors resolve by current membership.
3. The translation layer for the intercepted keys and the TUI layout key, attributed to the calling client.
4. `docs/spec/` updated (L2, L3, appendices) with a `docs/spec/CHANGELOG.md` entry; versioning per CONTRIBUTING.

Proof: protocol round-trip tests for every new variant; dispatch tests per command for grant, conflict, stale, and duplicate-operation refusals; an old client (pinned binary from the last release) creates, renames, and kills sessions against the new server in e2e.

## Stage 4: clients

Order matters: runtime and FFI first, because every client reads through them.

1. `phux-client-runtime`: snapshot plus journal reconciliation, operation correlation, retry with the same `OperationId`.
2. `phux-client-ffi`: lossless projection; Swift and C bindings regenerated.
3. `phux` CLI and `phux-mcp`: MIGRATION §04 and §05, including `rm`, `relaunch`, `restore`, and the kill workflow.
4. `phux-tui`: arrangement groups instead of windows; the verb table in MIGRATION §03.
5. Desktop, Cockpit, web: MIGRATION §06.
6. The "workspace" renames (MIGRATION §02), so stage 6 lands on a free name.

Deletes: client-side session lifecycle via metadata, the TUI layout metadata key (still translated server-side for old clients), the web client's `default` auto-create.

Proof: the close-versus-kill matrix runs against each client: close a view and the process survives; kill and it ends; exit a shell and the view goes; exit the last shell of a named session and the session stays. Cross-client: create in CLI, see it in TUI and desktop, rename on one, see it on the others.

## Stage 5: actors

1. Principal registry: the owner, plus owner-registered agent and automation credentials (building on ADR-0098's workload credentials).
2. `phux agent start` and in-session launches mint a delegated credential, own session, Observe and Interact, `on_behalf_of` the launcher.
3. Lease precedence for humans; `LeaseChanged` with epochs; evidence can answer who was driving when.
4. Every client shows who did it: "killed by claude (on behalf of you)".

Proof: an agent credential is refused `Terminate` outside its session; a human takeover from an agent always succeeds and the agent receives the lease-lost event; every journaled organization event has a principal.

## Stage 6: workspaces

`Workspace` records, link and unlink, the workspace arrangement, multi-authority observation in the client runtime, transfer, deleted-link and offline states. Gated on the runtime holding sessions from two authorities at once.

Proof: the workspace scenarios in the [domain model](FULL-DESIGN.md#07-workspaces) against two real servers, one killed mid-test.

## Stage 7: browser kind

[BROWSER-KIND.md](BROWSER-KIND.md), with a real owner-side browser driver. Not started before stage 4 is done.

## Working rules

- One writer per stage area; fan-out only along the crate seams in CONTRACTS §10.
- Every PR that changes behavior updates the doc that owns the behavior in the same PR.
- These research files freeze when the stage 0 ADRs merge; after that the ADRs and `docs/` are the source of truth.
