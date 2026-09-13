---
audience: contributors
stability: stable
last-reviewed: 2026-09-13
---

# 0118 — Agent integrations share a neutral Node runtime

**TL;DR.** Host-independent Node code for invoking the phux CLI, validating its
machine results, emitting AgentSession records, and projecting fleet context is
owned by the private `@phux/integration-runtime` module. Pi and OpenCode depend
inward on that module as sibling adapters; they never import one another. Each
public package remains independently installable by carrying the runtime in its
own artifact.

Status: Accepted
Date: 2026-09-13

## Context

The OpenCode plugin first reused `PhuxCli`, JSON schemas, and fleet awareness by
importing source from `integrations/pi`. AgentSession emission deepened that
dependency. The packed OpenCode artifact bundled the files and therefore had no
runtime dependency on `@phux/pi`, but source ownership still said that Pi was
the foundation and OpenCode was its consumer. That was false: the code knew
nothing about Pi and served two first-party host adapters.

## Decision

1. Host-independent Node implementation lives in the private
   `@phux/integration-runtime` module under `integrations/runtime`.
2. Pi and OpenCode are sibling adapters. Their source may import the runtime;
   their source may not import another host integration.
3. The runtime owns the bounded process runner, typed CLI adapter, JSON result
   validation, AgentSession emitter, and cache-preserving fleet awareness.
   Host event mapping, target persistence, commands, and tools stay with each
   adapter.
4. The runtime has direct type and unit gates. Both consumer packages retain
   packed-artifact tests.
5. The runtime is an implementation module, not a third public product. The
   OpenCode build inlines it; the Pi package carries it as a bundled dependency
   because Pi loads the package's TypeScript extension source directly.
6. `@phux/pi` keeps compatibility re-exports for its existing public Node
   interface. New sibling integrations import the neutral module directly.

## Why

The seam matches what varies. CLI invocation, result validation, lifecycle
emission, and fleet projection are invariant across hosts; event translation
and user interaction vary. Neutral ownership gives locality without forcing
users to coordinate versions or install another package. Direct runtime tests
make the shared behavior independently verifiable instead of testing it only
through whichever adapter happened to own the files.

## Tradeoffs

- Public artifacts contain the same runtime implementation more than once.
  Independent installation and versioning are worth that small duplication.
- Pi's compatibility files are intentionally shallow. They preserve an
  existing published interface; new code should use the neutral seam.
- A private module needs its own lockfile and gate even though it is never
  released independently.

## Alternatives

- **Keep Pi as owner and bundle its source into OpenCode.** Rejected: artifact
  independence does not repair the false source dependency.
- **Copy the implementation into each integration.** Rejected: fixes the arrow
  by losing locality; schema and security fixes would drift.
- **Publish a third required package.** Rejected for now: it exposes an
  implementation seam to users and couples installation to release ordering
  without adding capability.
- **Move every integration into one public package.** Rejected: Pi and OpenCode
  have independent host interfaces, dependencies, versions, and release lanes.
