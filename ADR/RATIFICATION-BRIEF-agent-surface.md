---
audience: contributors
stability: evolving
last-reviewed: 2026-08-12
---

# Ratification brief — the agent-surface batch (0075 / 0076 / 0077 / 0078)

**TL;DR.** This is a decision brief for bead phux-w7z2.1, not a decision. It
changes no ADR's status and ratifies nothing. It resolves or frames the five
overlaps between ADR-0075, ADR-0076, ADR-0077 and ADR-0078, supplies draft
wording the owner can adopt or discard, lists what ADR-0071 point 6 would owe
under its own point 7(c), and reports every place where those ADRs and the code
disagree. The largest finding is that ADR-0075's `%name` selector parses but is
wired to no verb, while two consumer docs and ADR-0071 point 6 already describe
it as live.

## 0. Standing

This document is deliberately not an ADR. It carries no `Status:` line, because
the ADR status vocabulary means "the repository has decided" and the repository
has not. Ratification is the owner's call. Everything below marked **DRAFT** is
text to paste into the named ADR *if* the recommendation is adopted; nothing
below has been applied to any ADR.

Read §1 first. Three of the five conflicts as filed are stale by one wave, and
one of them is stale in the direction that matters: an ADR the tree already
describes as shipped is not.

## 1. What the tree actually does

Verified by reading code, not ADR prose. Citations are file:line in this
worktree.

| ADR | Claim | Tree | Verdict |
|---|---|---|---|
| 0075 | `%name` resolves to one Terminal or refuses | `%` parses to `Selector::Agent` (`crates/phux-client/src/selector.rs:159`); `resolve_agent` / `resolve_agent_for_input` / `AgentIndex` exist and are unit-tested (`:560`, `:620`, `:338`) | **no production caller anywhere.** `resolve_with_tags` returns `Vec::new()` for `Agent` (`:320`), so every verb reports a plain selector miss (exit 1) |
| 0075 pt 2 | resolves against "the index `phux agent list` already builds" | `fetch_agent_index` (`crates/phux/src/commands/agent/record.rs:217`) returns a bare `HashMap`, best-effort, no completeness bit | the exit-3 partial-index refusal has **no source of truth** today |
| 0075 pt 4 | addressable grammar `^[a-z][a-z0-9_-]{0,31}$` | `is_addressable_agent_name` (`selector.rs:238`); duplicated as `is_addressable_name` (`crates/phux/src/commands/agent/start.rs:215`) | shipped, twice |
| 0076 | `agent prompt` on `APPLY_INPUT`, satellite refused, `--wait` edge-gated | `crates/phux/src/commands/agent/prompt.rs`, `crates/phux-client/src/agent_prompt.rs:520`, `:814`, `:830` | **shipped and wired** |
| 0076 pt 5 | no level fast path, ever | `EdgeTracker::new` records the baseline and never evaluates it (`crates/phux-client/src/agent_wait.rs:198-213`, `:219-246`); three tests pin it | **ADR and code agree** |
| 0077 | four additive `ScreenState` keys, `SCHEMA_VERSION` 3, `wait` unwraps | `crates/phux-core/src/screen.rs:38`, `:302`, `:318`, `:325`, `:335`, `:371`; `crates/phux-client/src/wait.rs:283` | **shipped**; already `Accepted` |
| 0078 | `snapshot --transcript`, `request_transcript`, a capability bit | zero code. `ServerFeature::KNOWN` is `AcknowledgedInput`, `FileUpload`, `MoveTerminal`, `TerminalReply`, `Shutdown` (`crates/phux-protocol/src/caps.rs:835`) | **nothing built**; `docs/consumers/agents.md:694` already says so |

The consequence for the batch is that this is mostly a ratification of shipped
surface, with one exception (0075) that is a commitment to build and one (0078)
that is a commitment to a multi-week subsystem.

## 2. Conflict 1 — the duplicate re-verify rule

**Recommendation: no merge. Ratify the wave-2 split as written, with two
edits.** The two rules stopped being duplicates in wave 2 and are now different
predicates with different subjects. ADR-0075 point 5 is a *safety gate*: it
reads the level of the withdrawn record shape (`kind` present **and**
`state: "unknown"`) and refuses, and it applies to every input-delivering verb
however the target was spelled. ADR-0076 point 4 is an *identity comparison*
against a caller-asserted `(kind, name)`, which shipped as
`agent send-keys --expect-agent/--expect-kind` long before either ADR and which
`prompt` inherits (`crates/phux/src/commands/agent/send_keys.rs:474`). The
single canonical wording for the doctrine underneath both already exists and is
normative: `docs/spec/L3.md` §3.7's level-versus-edge rule. So the ownership is
already correct — **0075 owns the write guard, L3 §3.7 owns the doctrine, 0076
owns only the comparison** — and ADR-0076 point 4 already cites 0075 point 5 and
already states the circularity the bead lists as open ("for `%name` the name came
out of the very record it re-reads"). That open item is stale.

The cost of the alternative — folding both into one ADR — is that you would move
a shipped flag pair into an ADR about names, and lose the split that makes the
crashed-pane consequence auditable: a safety gate is *allowed* to fire on a
corpse, an identity comparison is not.

Two edits are owed. First, 0075 point 5's verb list says "any ADR-0053
acknowledged-batch verb" without naming them; that set is now concrete. Second,
and more important, the guard has no caller.

**DRAFT — ADR-0075 point 5, first sentence and a new closing sentence:**

> **The write guard is a level read of the withdrawn shape.** `send-keys`,
> `paste`, `signal`, `run`, and every
> [ADR-0053](./0053-acknowledged-idempotent-input.md) acknowledged-batch verb —
> today `agent send-keys`, `agent prompt`, and `agent answer` — refuse a `%name`
> target whose record carries a `kind` **and** `state: "unknown"` […]
>
> The guard is written and unit-tested (`resolve_agent_for_input`,
> `crates/phux-client/src/selector.rs`) and is called by nothing: no verb
> branches on `Selector::Agent`, so a `%name` target falls through to
> `resolve_with_tags`, expands to the empty set, and fails closed as a selector
> miss. Ratifying this ADR commits to wiring it (phux-w7z2.3); it does not
> record that it is wired.

## 3. Conflict 2 — the record-name charset

**Recommendation: keep two grammars (ADR-0075 point 4 as written), and spend the
difference on the error message instead.**

Option A, the status quo, is that `docs/spec/L3.md` §3.7 keeps `name` as
"REQUIRED, non-empty" with no charset and `%` addresses a strictly smaller set.
Its cost is real and the ADR names it: some records are listed but not
`%`-addressable, and a user who reads `phux agent list` cannot tell which by
looking.

Option B, narrowing §3.7, costs three things and the bead names only two. It is
a normative change to a key whose schema §3.7 declares MUST NOT drift between
clients, so it needs a `docs/spec/CHANGELOG.md` row. It invalidates every
display-style name, "Claude Code" included. And — the cost neither the bead nor
the ADR states — `phux agent set --name` today accepts any non-empty trimmed
string (`crates/phux/src/commands/agent/record.rs:38-43`), so narrowing the spec
means adding a new refusal to a CLI surface ADR-0071 point 1 is about to freeze.
That is a breaking change on the consumer surface, owed the deprecation cycle in
`crates/phux/src/deprecations.rs`, immediately before a freeze, to buy tidiness.

Recommend A. The failure Option B is reaching for — a user types `%claude-code`,
gets "no agent named that", and cannot see that `@7` declares `"Claude Code"` —
is better fixed where the user is standing.

**DRAFT — appended to ADR-0075 point 4:**

> The two grammars must not fail silently past each other. Before reporting
> `Unknown`, resolution MUST scan the index for a live record whose `name`
> differs from the typed name only in characters the addressable grammar
> excludes — case, spaces, punctuation — and name it: `no agent named
> 'claude-code' on this hub; @7 declares "Claude Code", a valid record name that
> is not %-addressable — rename it with 'phux agent set @7 --name
> claude-code'`. This is a message, not a field: it teaches at the moment the
> user is wrong and adds nothing to the document surface ADR-0071 freezes.

## 4. Conflict 3 — the federation hole

**Recommendation: accept the documented hole for 1.0, do not schedule phux-2en,
and widen the ADR's statement of it, because the hole is the whole agent surface
rather than one verb.**

Verified. `ROUTE_INPUT` is relayed to satellites
(`crates/phux-server/src/hub/relay.rs:2124`, acked end-to-end in
`crates/phux-server/tests/hub_relay_federation.rs:328`), so `phux send-keys
host/@N` works. `APPLY_INPUT` is refused at three independent layers: it has no
relay arm (`relay.rs:2242`, pinned by
`route_to_satellite_ignores_local_and_unscoped_commands`), the server refuses a
non-local id before handoff (`crates/phux-server/src/runtime/commands.rs:2654`),
and the client refuses before opening a socket
(`crates/phux-client/src/agent_prompt.rs:814`). Separately, and more widely,
`phux.agent/v1` does not federate at all: metadata frames are top-level
`FrameKind` variants dispatched at `crates/phux-server/src/runtime/client.rs:1729`
and never reach `handle_command`, so they never reach `route_to_satellite` —
whereas `handle_subscribe_events` (`client.rs:2377`) does relay. ADR-0075
point 2's claim on this is exactly right.

So what a user hits, stated plainly: `send-keys`, `paste`, `signal` and `run`
reach `host/@N`; `agent prompt host/@N` exits 2 naming the satellite and the
direct-dial remedy; and `agent show`/`explain`/`set`/`clear`, `agent list`,
`agent wait` and `%name` are hub-local by construction. The dangerous one is
`agent wait host/@N`, which returns `AgentWaitError::NoRecord` — **exit 2, "no
agent record"** — reporting a healthy remote agent as *undeclared* rather than as
*unreachable*. No test pins that path.

The alternative — scheduling phux-2en into 1.0 — is not 1.0-sized. It is P3 and
labelled post-1.0, and ADR-0053 point 7 already declined federated acknowledged
input for the reason that still holds: it needs destination capability
negotiation, destination-owned dedupe, and `server_id` incarnation semantics
across the hop. Buying uniformity there costs the idempotency guarantee that is
the whole point of the verb.

**DRAFT — ADR-0076 point 1, replacing its last two sentences:**

> `APPLY_INPUT` is local-only ([`L1.md`](../docs/spec/L1.md) §6.2.1, ADR-0053
> point 7), so a satellite target ([ADR-0066](./0066-host-namespace.md)) is
> refused (exit 2), never downgraded to `ROUTE_INPUT`. `agent wait` is refused
> for a different reason with the same effect: metadata frames are dispatched
> outside `handle_command` and `phux-server/src/hub/` has no metadata arm, so a
> hub answers `GET_METADATA` for a satellite pane with `None`. **The hole is the
> whole agent surface, not one verb**, and this ADR owns saying so:
> `send-keys`, `paste`, `signal`, and `run` reach `host/@N` through the
> `ROUTE_INPUT` relay, while `agent prompt`, `agent wait`,
> `agent show`/`explain`/`set`/`clear`, `agent list`, and `%name` are hub-local.
> Closing it is phux-2en, post-1.0. Until then a satellite refusal MUST name the
> satellite and the direct-dial remedy rather than reporting absence: `agent wait
> host/@N` answering `no agent record` misreports a live remote agent as
> undeclared and owes a distinct not-federated refusal.

## 5. Conflict 4 — two wait verbs

**Recommendation: keep the split. Both have shipped, both are already inside
ADR-0071 point 6, and ADR-0065 does not ask for what folding them would buy.**

ADR-0065's one-grammar principle is about one *spelling per thing* — one
`--socket`, one `--split`, one alias pair, one JSON error shape. All seven of
its decision points are about flag spelling; none is about verb count. Its own
remedy for a collision is a disambiguating noun, which is what `agent` is here.

The two verbs differ in every dimension a caller branches on. Data source:
`phux wait` polls `GET_SCREEN` and matches over `match_lines`
(`crates/phux-client/src/wait.rs:283`); `agent wait` runs `SUBSCRIBE_METADATA`
plus a `GET_METADATA` baseline and tracks edges. Condition domain: three screen
conditions — `Contains`, `Matches`, `Idle` (`wait.rs:148-176`) — against a closed
four-word state vocabulary. Satisfaction predicate: a level match against the
observed transition `L3.md` §3.7 *requires* of a completion gate. Exit-code
contract: `agent wait` adds departure as `1` and an absent record as `2` on top
of the shared `0`/`124`/`2`/`1`. Folding them yields one verb whose `--until`
carries two value domains, disambiguated by the presence of a different flag —
the grammar-plus-a-footnote that ADR-0075's own Why rejects.

The cost of the alternative is concrete because both verbs shipped: a
deprecation cycle on stable surface immediately before the freeze, a rewrite of
ADR-0071 point 6, `docs/consumers/agents.md` §5.2, and the MCP tool freeze in
ADR-0071 point 7(b), which already names `phux_agent_wait` as one of the ten
frozen tools. That is a large, visible break to remove one verb.

**DRAFT — ADR-0076 Alternatives, replacing the "Extend `phux wait`" entry:**

> **Extend `phux wait` with an agent-state condition.** Rejected on four
> measured differences rather than on taste: the data source (a `GET_SCREEN`
> poll over `match_lines` versus `SUBSCRIBE_METADATA` plus a `GET_METADATA`
> baseline), the condition domain (free text or regex versus the closed
> `idle|working|blocked|done` vocabulary), the satisfaction predicate (a level
> match versus the observed transition [`L3.md`](../docs/spec/L3.md) §3.7
> requires of a completion gate), and the exit-code contract (`agent wait` adds
> departure as `1` and an absent record as `2`). One verb carrying both needs
> `--until` to mean two things, disambiguated by the presence of another flag.
> [ADR-0065](./0065-one-cli-grammar.md) asks for one spelling per thing and
> prescribes a disambiguating noun where two things collide; `agent` is that
> noun. Both verbs have shipped and both are enumerated in
> [ADR-0071](./0071-what-phux-1-0-commits-to.md) point 6, so folding them now
> would spend a deprecation cycle on stable surface immediately before the
> freeze.

**DRAFT — ADR-0076 point 5, two factual corrections:** `--timeout MS` is wrong;
the shipped flag is `--timeout SECS` (`crates/phux/src/commands/agent/mod.rs:141`,
converted with `Duration::from_secs` at
`crates/phux/src/commands/agent/wait.rs:136`), and ADR-0071 point 6 already says
SECS. And after "an unknown spelling is a usage error (exit 2)", add: "`--until
done` *alone* is likewise a usage error (`unsatisfiable_wait`, exit 2), since no
shipped writer emits it; the default set is exempt, degrading to `idle,blocked`
on an uninstrumented pane."

## 6. Conflict 5 — the normative spec contradiction

**Plain answer: neither the spec nor ADR-0077 is wrong today. The premise is one
wave stale.**

Wave 2 moved the alternate-screen harvest out of ADR-0077 into ADR-0078.
ADR-0077 as it now stands makes no spec claim, no wire change, and no PTY write,
and everything it asserts is in the tree: `SCHEMA_VERSION` 3
(`crates/phux-core/src/screen.rs:38`), the four keys all with `#[serde(default)]`
(`:302`, `:318`, `:325`, `:335`), `has_soft_wrap_info()` (`:371`), and `wait`
matching over `unwrapped_rows()` (`crates/phux-client/src/wait.rs:283`). Nothing
in the tree writes to a pane on a `GET_SCREEN`: `request_transcript` occurs
exactly once in the repository, in ADR-0078's own prose, and there is no
transcript capability bit (`crates/phux-protocol/src/caps.rs:835`).
`docs/consumers/agents.md:694` already tells consumers the harvest is proposed
and unusable. So `L1.md` §6.1's "with no side effects" is **currently true** and
the contradiction is **not live in the tree**.

It becomes live only if ADR-0078 is accepted *and* implemented. Accepting 0078
is therefore a commitment to a future normative spec change, not the ratification
of an existing one — and the minimal correct fix is to make that amendment in the
PR that ships the first harvesting code, not at ratification, so the spec never
describes a behaviour the server does not have.

**DRAFT — the exact amendment, for ADR-0078 point 3 to name:** three sentences
change, in one PR, plus one `docs/spec/CHANGELOG.md` row for the new
`ServerFeature` bit (additive under ADR-0061 §2, so no `PROTOCOL_VERSION` move
and no change to the head version the `spec-version-sync` gate compares).

> `L1.md` §6.1, first sentence — "`GET_SCREEN` reads a Terminal's current
> viewport as structured data with no side effects" becomes "…with no side
> effects **except when `request_transcript` is set, which drives the
> application's own scrollback and is PRIMARY-only (ADR-0078)**".
>
> `L1.md` §6.1, closing clause — "Allowed for viewers." becomes "Allowed for
> viewers **except with `request_transcript`, which requires PRIMARY**."
>
> `L1.md` §6.2, closing clause — "the read-only `GET_SCREEN` remains the
> viewer-safe surface" becomes "the read-only `GET_SCREEN` remains the
> viewer-safe surface **for every request that does not set
> `request_transcript`**".

**On ADR-0077 conflict 3 (ADR-0046 point 8 versus the harvest's idle gate):
resolved correctly by ADR-0078 point 5, and the residual has no structural
fix.** `L3.md` §3.7 expressly permits a safety gate to read the level and names
"refusing to scroll a screen that may be repainting" as its example — 0078 point
5 is quoting the normative rule that was written for it. The residual is that a
consumer can explicitly declare `idle` on a busy agent (ADR-0046 point 8 is real
in the code: `agent_state.rs:148` marks the terminal declared, and the detector
hard-returns at `runtime/client.rs:303`) and thereby unlock a harvest. There is
no available mitigation, because §3.7 states that a consumer "neither can nor
needs to distinguish" a declared record from a derived one — the record carries
no provenance field, and minting one adds a field to a document ADR-0071
freezes. ADR-0078's Tradeoffs already names this; recommend accepting it rather
than inventing provenance.

**One understated cost in ADR-0078 point 6, which the owner should price before
accepting.** Point 6 says detector publication freezes "by extending ADR-0046's
existing `skip-state-update` clause […] rather than by inventing a second freeze
the implementation would then build twice." The shipped mechanism is not
extensible that way: `skip_state_update` is a per-rule boolean on a *main-screen
text* rule (`crates/phux-server/src/agent_detect/rules.rs:202`, shipped as
`rules/claude.toml:106` matching the literal string "showing detailed
transcript"), OR-ed into an evaluation `freeze` flag at `rules.rs:636` and
consumed in `AgentDetector::tick` at `agent_detect/mod.rs:496`. There is no
alternate-screen bit and no way for the actor to assert the freeze for a
duration. A traversal needs an actor-level, duration-scoped suppression. That is
new mechanism, not reuse, and it belongs in point 6's wording and in the
1,760-line estimate.

## 7. What ADR-0071 point 6 would owe, per point 7(c)

Point 7(c) requires anything ratified out of 0075/0076/0078 to be carved into
point 6 in the same PR that ships it. Point 6 has already absorbed more than the
bead assumes.

**Already carved in, so 7(c) is partly discharged:** `agent prompt TARGET TEXT`
with `--expect-agent`, `--expect-kind`, `--wait`, repeatable `--until`,
`--timeout`, `--json`; the `agent prompt` receipt v1; `agent wait [TARGET]
--until STATE... --timeout SECS --json` with the closed `idle|working|blocked|done`
vocabulary and the `idle,blocked,done` default; `%name` selectors "over explicit
names only"; and `ScreenState`'s four additive keys at `SCHEMA_VERSION` 3, cited
to ADR-0077.

**Owed if ADR-0075 ratifies:**

1. The addressable-name grammar `^[a-z][a-z0-9_-]{0,31}$` as a parse-time
   refusal (exit 2). It is a CLI-visible grammar and point 6 does not list it.
2. The four `%name` refusals and their statuses: ambiguous → 2, kind-constant →
   2, withdrawn → 2, partial index → 3.
3. **A collision that must be resolved in the same edit.** Point 6's closing
   clause says `agent wait` and `agent send-keys` "use the shared resolver, so a
   partial-fleet miss is `1` carrying `partial_view` in `error.code` rather than
   the `3` the rest of the `agent` family spends." `%name`'s partial-index
   refusal is `3` in `AgentResolveError::exit_code`
   (`crates/phux-client/src/selector.rs:458`). Those two rules meet on
   `phux agent send-keys %foo …`. Point 6 must say which wins. See Q2.
4. Error codes for those refusals in `commands::json_err::codes`, which point
   7(a) requires of every stable code. None exists today.

**Owed if ADR-0076 ratifies:** only the `--until done`-alone usage refusal and
its `unsatisfiable_wait` code. Everything else is present.

**Owed if ADR-0078 ratifies:** `snapshot --transcript` and its conflict set; the
`transcript` payload object (rows, status, refusal reason, `seam_count`) as an
additive `ScreenState` key held at `SCHEMA_VERSION` 3; the closed refusal-reason
vocabulary from point 4, `no_detector`, `agent_not_idle` and `not_local`
included; the exit-`0`-with-a-named-reason contract, which is a genuinely new
exit-code semantic — `crates/phux/src/exit_codes.rs` has no precedent for a
refusal that is not an error; and the *recorded absence* that MCP's
`phux_snapshot` does not gain the flag (point 8), written into point 6 so a later
reader cannot "discover" it post-freeze.

**Owed if ADR-0077 ratifies:** nothing. It is already Accepted and already
carved.

## 8. What the owner must decide

Recommended answers are marked **[R]**.

1. **Ratify as one batch, or split?** **[R] Split three ways.** 0077 is already
   Accepted and shipped. Ratify 0075 and 0076 together — they share the `%name`
   and re-verify seam and their edits land in one PR. Hold 0078 separately.
   Batching 0078 with them makes the cheap, shipped, correct part wait on a
   three-week subsystem whose empirical prerequisite is undischarged; that is
   the same mistake wave 2 corrected when it split 0078 out of 0077.
2. **Does `%name`'s partial-index refusal spend exit 3 on `agent send-keys` /
   `agent wait` / `agent prompt`, against ADR-0071 point 6's "keep 1"?** **[R]
   Yes, 3 wins for `%name`, and point 6 is amended to say so.** They are
   different misses: a partial *fleet* is a server-side gap the client infers,
   while a partial *index* is the client knowing it did not finish looking — and
   `%name` is the one selector whose entire contract is singularity, so "I could
   not check for a second holder" is materially different from "there is none".
   The alternative — clamp to 1 with `partial_view` in `error.code`, uniform
   with the rest of the family — is defensible and costs the retry signal
   exactly where retry is most obviously right.
3. **Ship `%name` before or after the ADR-0046 occupant-change guard
   (phux-w7z2.27)?** **[R] Ship; the guard is already in the tree.**
   `apply_agent` emits `DetectOutcome::Reidentified`
   (`crates/phux-server/src/agent_detect/mod.rs:740`), the drain writes one
   corrective `SET` landing on `unknown` and never a tombstone
   (`runtime/client.rs:232-278`), and retraction is gated on two consecutive
   positively-observed vacancies (`agent_detect/mod.rs:125`). The residual holes
   are the ~5 s identity cadence and the preserved explicit `kind`, both filed
   (phux-w7z2.43, phux-w7z2.45) and both already stated in 0075 point 6.
4. **Do you accept that phux's own Claude integration yields zero
   `%`-addressable panes?** **[R] Yes, and say it in point 7(a).** The shim
   writes `--name claude --kind claude`
   (`crates/phux/src/commands/agent/shim.rs:398-408`), which is exactly the
   kind-constant shape `resolve_agent` refuses (`selector.rs:641`). So `%claude`
   refuses on every shim pane and `%name` is inert until a human runs `phux agent
   set @N --name <name>`. That is the correct behaviour — a kind is not a name —
   but "the feature does nothing until you rename something" is a product fact
   the ADR currently leaves to inference.
5. **Drop `done` from the default `--until` set (phux-w7z2.28)?** **[R] Keep it,
   and document the shipped asymmetry instead.** `--until done` alone is already
   refused as `unsatisfiable_wait` (`crates/phux/src/commands/agent/wait.rs:60-78`)
   while the default set containing it is exempt — which is the right shape, since
   an explicitly unsatisfiable request is a usage error and a default that
   degrades to `idle,blocked` is not. It appears in no ADR and in no ADR-0071
   enumeration. Dropping `done` instead costs the forward compatibility of an
   instrumented agent that does write it, for no gain the exemption does not
   already deliver.
6. **Does ADR-0075 point 5's guard also cover `attach` and `kill`?** **[R] No,
   and say so rather than leaving it to a verb list.** `attach` writes no bytes.
   `kill` targets the *pane*, not the occupant; refusing to kill a pane whose
   record was withdrawn would strand exactly the panes a user most wants gone.
7. **Accept, reject, or defer ADR-0078?** **[R] Defer — leave it Proposed and do
   not ratify it in this batch.** Its own Context says to capture live viewports
   before accepting, and that has not happened; its point 6 freeze mechanism is
   new mechanism rather than the extension it claims (§6 above); and nothing else
   in the batch depends on it — ADR-0077 says so in as many words. Deferring
   costs nothing under point 7(c), which only requires a carve-in in the PR that
   *ships* it.
8. **Split 0076 and 0078 against the ~150-line cap?** **[R] Yes, at
   ratification.** They are 224 and 214 lines; no CI gate enforces the cap, but
   `docs/CONVENTIONS.md` prescribes the remedy — a companion page under
   `docs/architecture/`. The natural cuts are 0076's points 2 and 7 (the
   receipt-reading table and the `--json` document) and 0078's points 1, 6 and 7
   (the phase machine, the payload shape, the restore primitives).
9. **Fold ADR-0036's two corrections (phux-w7z2.22, phux-w7z2.16) into the same
   PR?** **[R] Yes.** They are in the same seam — a `phux-ask` sentinel tier that
   is not in `AskedSource`, and a closing line about waiting for libghostty to
   expose OSC 9 that phux's own raw-byte scanner has made false — and leaving
   them makes the agent-detection story incoherent at the freeze.

## 9. ADR-versus-code discrepancies

Every item below was read in the tree.

1. **ADR-0075 is unusable end-to-end while two consumer docs and ADR-0071
   point 6 describe it as live.** No verb branches on `Selector::Agent`; `%name`
   is a guaranteed selector miss on every verb and every MCP tool. Yet
   `docs/consumers/agents.md:714` and `docs/consumers/mcp.md:131` list `%name`
   in the working selector grammar with no caveat, and ADR-0071 point 6 already
   carves it into the 1.0 freeze. This is the ADR-mechanism-never-implemented
   pattern, live, on a surface about to be frozen.
2. **ADR-0075 point 2's index claim is false in the way that matters.**
   `fetch_agent_index` (`crates/phux/src/commands/agent/record.rs:217`) is
   explicitly best-effort and returns a bare `HashMap`; it cannot express the
   complete/partial bit that `resolve_agent`'s exit-3 refusal is defined in terms
   of. The refusal has no source of truth today.
3. **ADR-0075 point 3's "would narrow silently" is not what today's code does.**
   `resolve_one` does apply `pick_target_pane` unconditionally, but
   `resolve_targets` yields the empty set for `Agent`, so the result is a miss,
   not a wrong pane. The narrowing risk arrives the moment someone wires `Agent`
   into `resolve_targets`. Also: there are **two** copies of `resolve_one`
   (`crates/phux-mcp/src/tools.rs:743` and `crates/phux-mcp/src/ask_tool.rs:63`,
   byte-identical bodies); the ADR names one, and a migration must touch both.
4. **ADR-0076 point 5 says `--timeout MS`; the shipped flag is SECS**
   (`crates/phux/src/commands/agent/mod.rs:141`, `agent/wait.rs:136`), and
   ADR-0071 point 6 already says SECS. The ADR is the one that is wrong.
5. **A shipped CLI refusal appears in no ADR:** `--until done` alone is rejected
   up front as `unsatisfiable_wait`, exit 2, with the default set exempt
   (`crates/phux/src/commands/agent/wait.rs:60-78`, `:113-115`).
6. **`ADR/README.md`'s row for ADR-0076 is stale**, saying "the `wait` half
   shipped, the `prompt` half has not". `agent prompt` is fully wired and tested.
   (Not corrected here: the index is being touched concurrently by another
   agent.)
7. **`AgentResolveError::exit_code` hard-codes `1`/`2`/`3`** and does not
   reference `crates/phux/src/exit_codes.rs`, so there is no drift guard between
   the resolver's statuses and the published exit-code table.
8. **A server error message names the wrong frame.**
   `with_route_input_destination` is shared, so a satellite-targeted
   `APPLY_INPUT` is refused with `"ROUTE_INPUT to satellite route unsupported"`
   (`crates/phux-server/src/runtime/commands.rs:2657`). The `ErrorCode` is right;
   the message will mislead anyone reading a server log.
9. **ADR-0078's freeze mechanism is not the extension it claims** — see §6. The
   shipped `skip_state_update` is a main-screen text-rule boolean with no
   alt-screen bit and no actor-assertable duration.
10. **No test pins `phux agent wait host/@N`.** The non-federation of metadata is
    well established at the server layer; that it surfaces to the user as
    `NoRecord`/exit 2 rather than as an unreachable-satellite refusal is inferred
    from the code path, not asserted anywhere.
11. **The bead's own conflict list is stale in three places**, noted so the owner
    does not re-litigate: ADR-0076 point 4 already cites ADR-0075 point 5 and
    already states the `%name` circularity; the shipped `EdgeTracker` already
    implements the strict no-fast-path rule the current ADR text specifies, so
    there is no ADR-versus-code divergence left there; and `phux wait` has three
    `Condition` variants (`Contains`, `Matches`, `Idle`), not two.

## 10. Not determined

- Whether the server actually populates `soft_wrap`'s index vectors from
  libghostty's per-row bit was not traced end to end. `snapshot` re-bases the
  indices after a `--tail` clip, which implies a producer, but the producer path
  was not read.
- Nothing here was executed. Every claim above is static reading of this
  worktree at `origin/main`.
