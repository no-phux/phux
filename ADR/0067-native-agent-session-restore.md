---
audience: contributors, agents
stability: stable
last-reviewed: 2026-08-01
---

# 0067 — Native agent-session restore

**TL;DR.** A `phux launch` may establish one provider-native session identity,
record only its bounded opaque identity plus owning plugin/integration on the
spawned Terminal, copy that inert record into workspace archive schema 2, and
rebuild resume argv from the current owning integration when the pane is
restored. The bridge composes existing L3 metadata and spawn operations; it adds
no wire frame and never archives executable argv as resume authority.

Status: Accepted
Date: 2026-08-01

## Context

ADR-0040 records what agent occupies a live pane, but that `phux.agent/v1`
identity is descriptive and dies with the Terminal. ADR-0042 can launch an
integration, and workspace archives can create a replacement seed process, but
neither surface retained the provider-native identity needed to resume Claude,
Codex, Gemini, or another CLI after the old PTY was gone.

Persisting the old command is insufficient and unsafe. Integration packages can
be upgraded or disabled, wrapper paths can move, and archived strings must not
become a second shell or executable trust boundary. A resume record also cannot
be global: two panes may run the same integration with different native
sessions.

## Decision

1. **Separate live identity from resume identity.** `phux.agent/v1` remains the
   public name/kind/state record from ADR-0040. `phux.agent-session/v1` is an
   opaque, Terminal-scoped L3 record containing only `plugin_id`,
   `integration_id`, and `native_id`.
2. **Templates own native session policy.** An integration's
   `[session_identity]` may declare a dedicated `PHUX_*_SESSION_ID`
   `native_env`, structured `resume_args`, and structured `fresh_args`. Each
   argument vector contains exactly one standalone `${PHUX_AGENT_SESSION_ID}`
   argument. Expansion replaces only that opaque argv element; option-shaped
   identities and templates that position identity as executable or evaluator
   source are rejected. A fixed plugin-owned interpreter script remains valid.
3. **Launch establishes identity only when the provider supports it.** A
   caller-supplied `native_env` value resumes that exact identity. Otherwise a
   UUID is generated only when `fresh_args` documents a caller-supplied fresh
   identity. Providers without that facility launch normally and remain
   non-restorable unless an existing native identity is supplied explicitly.
4. **Spawn and record publication are one server state transaction.** Launch
   carries the encoded record in additive optional `SPAWN_TERMINAL` field 9.
   The server validates its 1–4096-byte envelope and stores it under the new
   local Terminal before releasing the state lock; actor-build failure reaps
   both the Terminal and metadata. Launch reads back the exact scoped bytes
   before succeeding. An older server may ignore field 9, so launch falls back
   to bounded ordinary SET/GET confirmation; failure attempts to kill the new
   Terminal. Satellite spawns reject local provenance.
5. **Workspace archive schema 2 is the durable boundary.** Save reads the
   record from each exact pane and copies valid records into that pane's
   `agent_session` field. It verifies that the local Terminal set did not change
   during those reads rather than silently archiving a pane whose record was
   reaped. Schema 1 remains readable and has no native restore semantics.
   Invalid, padded, option-shaped, control-bearing, or oversized identities fail
   closed.
6. **Restore re-resolves trusted code.** For the preferred pane of each missing
   archived session, restore resolves the current enabled integration by
   `integration_id`, verifies that its `plugin_id` still matches the archived
   owner, and rejects duplicate enabled plugin owners. It validates current
   resume policy and reconstructs argv, environment, and working directory from
   the current template plus opaque `native_id`. Missing, disabled, renamed,
   ambiguous, ownership-changed, or no-longer-restorable integrations fail
   before creating that session. Native restore first probes the reserved
   `phux.session.created/v1/<request-token>` namespace and rejects an older
   server before creating anything if that namespace is still client-writable.
   Session creation then returns through a nonce-correlated, owner-only,
   one-shot L3 result, so another client cannot observe, overwrite, delete, or
   subscribe to the result, and a concurrent same-name loser cannot stamp
   provenance onto the winner's Terminal. The replacement Terminal receives
   and confirms the same L3 record; confirmation failure removes it.
7. **Bounds are part of the trust boundary.** Plugin and integration ids are
   trimmed, control-free UTF-8 of at most 120 bytes. Native ids are trimmed,
   non-option-shaped, control-free UTF-8 of at most 1024 bytes. The complete
   encoded record is at most 4096 bytes on atomic and ordinary metadata writes.
   Identity environment names use only the dedicated `PHUX_*_SESSION_ID`
   namespace. Archive data never supplies an executable path, interpreter
   source, or arbitrary resume argv.

## Consequences

- A pane restart can replay the exact provider-native identity while still
  using the currently installed wrapper and CLI policy.
- Archive schema 2 and optional `SPAWN_TERMINAL` field 9 are additive data
  format changes, not a protocol version change. Existing schema-1 archives
  restore with fresh-process behavior. Fresh session creation remains
  compatible with older servers; native restore fails before creation when its
  capability probe detects that the server cannot atomically install the
  restore record.
- Restore remains session-seed reconstruction, not PTY resurrection. Existing
  session names are still skipped and never have their running process
  replaced.
- Native restore is local-terminal only in this decision. Satellite archive
  projection remains bounded by the existing federated state/metadata model.
- Integrations must publish source-verified CLI flags. A guessed fallback or a
  provider switch in core is not acceptable.

## Alternatives rejected

**Archive the resolved launch argv.** Rejected because stale archive content
would become executable authority and would bypass current package ownership
and policy.

**Add provider-specific resume branches to `phux workspace restore`.** Rejected
because it duplicates integration packaging, drifts as CLIs change, and moves
untrusted provider knowledge into core.

**Put resume fields into `phux.agent/v1`.** Rejected because a public mutable
state/label record has different ownership and lifetime from inert resume
provenance. Coupling them would let ordinary lifecycle writers alter restore
authority.

**Add a protocol frame or server database.** Rejected because Terminal-scoped
L3 metadata already supplies the exact live ownership boundary and workspace
archives already supply the durable boundary. A new server-side store would add
migration and synchronization semantics without improving correctness.
