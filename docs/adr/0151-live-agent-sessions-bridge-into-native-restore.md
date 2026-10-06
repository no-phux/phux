---
audience: contributors, agents
stability: stable
last-reviewed: 2026-10-06
---

# 0151 — Live agent sessions bridge into native restore

**TL;DR.** `phux workspace save` also archives an ADR-0068 resume record for
a pane whose agent was started in an ordinary shell: when the pane's unique
live `AgentSession` names a provider-native id and exactly one enabled
integration claims that provider as its `[agent_identity] kind` and declares
native resume, save records that integration's owner ids plus the opaque
native id. Restore is unchanged and still rebuilds argv from the current
integration.

Status: Accepted
Date: 2026-10-06

## Context

[ADR-0068](./0068-native-agent-session-restore.md) writes the inert
`phux.agent-session/v1` record only on a Terminal that `phux launch` spawned.
Most agents are not launched that way: Claude Code's hook shim, the pi and omp
extensions, and the OpenCode plugin run inside a shell the user already had,
and each opens an `AgentSession` child with `--provider` and `--native-id`
([ADR-0103](./0103-agent-session-resource-and-producer-fed-streams.md)). Save
read only the launch record, so those panes were archived with no
`agent_session` and came back after a restart as bare shells, although the
provider id and an installed integration able to resume it were both known.

## Decision

1. **Save derives the record from the live session.** For each local pane
   whose unique `AgentSession` child carries a native id, save resolves the
   child's provider as a detection kind with the `phux agent start --kind`
   rule: the one enabled integration whose `[agent_identity] kind` claims it,
   else an integration whose id is the provider. Several claimants are
   ambiguous and bridge nothing.
2. **Only a resumable owner qualifies.** The resolved integration must declare
   provider-native restore (`resume_args`). The archived record is the
   ADR-0068 record, `plugin_id`, `integration_id`, `native_id`, validated by
   the same bounds; no argv, executable, or provider name is archived.
3. **A launch record keeps precedence.** A pane that already has
   `phux.agent-session/v1` keeps it. When the live session resolves to the
   same owner with a different native id, the live id replaces the recorded
   one: the provider moved to a new conversation in that pane.
4. **Unbridgeable sessions warn.** A pane with no record whose live session
   cannot be bridged (no claimant, ambiguous, not resumable, invalid id) is
   archived as before, and save prints one warning naming the pane, provider,
   and reason.
5. **Templates ship the policy.** The first-party agent-tools plugin claims
   `claude`, `pi`, `omp`, and `opencode`, each with source-verified resume
   flags; fresh-identity flags are declared only where the CLI accepts a
   caller-chosen id.

## Why

The `AgentSession` already is the authoritative live statement of which
provider conversation a pane hosts, written by the integration that owns the
provider. Routing its id through the existing kind claim and the existing
ADR-0068 record keeps one restore path, one trust rule (argv comes only from
the current enabled template), and no provider knowledge in core.

## Tradeoffs

- A user without an enabled integration claiming the provider still restores
  a shell; the save-time warning says so instead of failing.
- Save now reads plugin configuration once per distinct provider.
- The record is only as current as the last `workspace save`; a conversation
  switched after the save resumes the archived one.

## Alternatives

**Write `phux.agent-session/v1` from each integration.** Rejected: every
integration would need plugin and integration ids it does not know, and the
record's writer set would grow beyond launch and restore.

**Archive the provider slug and resolve it at restore.** Rejected: it adds a
second archive shape and defers the ownership check past the point where the
user can see the save warning; ADR-0068's record already carries what restore
needs.
