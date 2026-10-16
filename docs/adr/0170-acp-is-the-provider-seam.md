---
audience: contributors
stability: stable
last-reviewed: 2026-10-15
---

# 0170 — ACP is the sole provider seam

**TL;DR.** Host-owned agent sessions use the pinned official Rust Agent Client
Protocol client. Native ACP servers and operator-installed maintained adapters
replace provider SDK bridges. Existing terminal integrations remain separate.

Status: Accepted
Date: 2026-10-15

## Context

Desktop and mobile need the same durable agent, structured events, and explicit
human permissions without each frontend owning a provider process. Maintaining
separate Node SDK adapters and custom OMP RPC duplicates protocol contracts,
introduces a private runtime and generated schemas, and makes capability claims
hard to verify. Several harnesses already supply ACP; others have maintained
adapters outside phux.

## Decision

Use `agent-client-protocol = "=0.10.4"` as the only provider seam. This is the
official Rust client; the exact pin and lockfile constrain protocol changes.
Its futures run on a local runtime in the durable guardian worker, never the
frontend or daemon event loop. The retained broker owns activation fencing,
permission ownership, operation deduplication, and same-worker upgrade.

Public launch uses `--acp` and integration manifests declare `[acp]` with safe
explicit argv and prerequisites. Operators install and authenticate native ACP
servers or maintained adapters. phux does not auto-download adapters, embed a
Node runtime, or accept provider licenses. Provider-specific SDKs, generated
Codex schemas, the private Node bridge, and OMP RPC are removed.

Capabilities reflect the pinned protocol and actual negotiation. Client file and
terminal callbacks are not advertised and fail explicitly. Stable session restore
requires `loadSession`; ACP stop reasons and errors are not fabricated admission
or success receipts. Human-only answers retain broker ownership across viewers.
Unknown command outcomes never cause automatic prompt replay.

## Why

One official protocol client reduces the code and security surface while enabling
arbitrary configured agents. The dependency is justified by replacing multiple
provider SDK dependencies and hand-maintained protocol code; `async-trait` and
Tokio compatibility support the official client's local futures and I/O. Existing
server ownership, native UI, and UniFFI remain provider-neutral.

## Tradeoffs

An ACP initialize handshake proves compatibility, not model-turn parity. Adapters
have their own licenses and version ranges. Optional mode/model/config surfaces,
rich attachments, and client-executed filesystem/terminal tools are not exposed
as phux controls. The pinned standard has no general structured elicitation
callback; explicit supported extensions remain distinct from standard ACP.
Aider and other unverified agents remain terminal-only.

## Alternatives

Provider-specific SDK bridges offer broader provider-specific APIs but require a
private runtime, custom schema maintenance, and multiple permission semantics.
This design deliberately rejects that maintenance burden.

Terminal scraping preserves interactive CLIs but cannot supply reliable typed
permission, cancellation, or reconnect semantics. Attached TUI integrations stay
available without pretending to be ACP sessions.
