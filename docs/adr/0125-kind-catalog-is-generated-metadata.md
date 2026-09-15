---
audience: contributors
stability: stable
last-reviewed: 2026-09-14
---

# 0125 — The kind catalog is generated metadata, not wire

**TL;DR.** One compiled Rust table, `phux_protocol::kinds`, holds what each
resource kind answers and which closed verb every client frame and command
needs. Discovery (`phux --capabilities --json`, `docs/reference/kinds.md`)
and the dispatch classifier read the same rows, so they cannot disagree.
There is no catalog frame, no runtime method registry, and no generic
invoke verb. Discovery never grants authority.

Status: Accepted
Date: 2026-09-14

## Context

PHA-406 asks for capability discovery per resource kind. phux already
negotiates everything that has to be negotiated: the `HELLO_OK` feature bits
and the open `ResourceKind` tag
([ADR-0061](./0061-capabilities-add-versions-break.md),
[ADR-0102](./0102-resources-the-server-serves-kinds.md)). What it lacks is a
machine-readable answer to two questions: which methods a Terminal answers,
and what authority each one needs. The facet table in `docs/spec/L1.md` §1.1
is prose. The total classification in `docs/spec/workload-auth.md` §6 exists
only in the spec. With no Rust classifier, the dispatch choke point planned in
phux-pjc5.5 has nothing to consume, and nothing stops a new command from
shipping unclassified.

The prototype this program extracts from discovered methods at runtime and
invoked them by string. PHA-406 names that shape as the ambient-authority
trap to avoid. A peer that can enumerate and call arbitrary names needs a
second authorization model for those names, and a read-only introspection
call becomes a way to learn a write and then reach it.

## Decision

1. `crates/phux-protocol/src/kinds.rs` holds one static table. `Verb` is the
   six closed bits of workload-auth §5, byte-equal. A `Rule` is one row of
   a §6 table: the spec's case text, a `Requirement` (verbs, exemption,
   nested, or deny), and a closed `Subject`. `Subject::Global
   { owner_uds_only }` expresses the owner-socket transport predicate, and
   `Subject::ParentOfNamed` the producer-append rule. A `MethodSpec` names a
   method by its wire message or metadata key and gives its carrier, every
   row an instance can land on, its gating `ServerFeature`, and whether it
   has shipped. A `KindSpec` groups facet methods, events, and
   resource-scoped metadata keys per `ResourceKind`. Server-level and
   substrate methods sit beside the kinds.
2. `classify_frame(&FrameKind)` and `classify_command(&Command)` return one
   of those rows. Both match exhaustively inside the defining crate, so a
   new variant is a compile error until it is classified. Unknown, retired,
   unallocated, and wrong-direction frames and tags are `Deny`. The
   `COMMAND` envelope defers to its nested tag and grants nothing by
   itself. A command the spec does not list is `Deny` until the spec gives
   it a row.
3. Tests pin the table to the spec in both directions. A golden compares
   every §6 row's case, requirement, and subject. One sample per `Command`
   variant and per client-originated `FrameKind` variant, with every
   payload-dependent split, asserts the exact row it lands on and that its
   catalog method names that row. The Terminal and AgentSession facets
   equal L1 §1.1, and a tag sweep proves the catalog names exactly the
   command tags the decoder accepts.
4. Discovery surfaces read the table. `phux --capabilities --json` gains
   `kinds`, the build-time catalog, and `docs/reference/kinds.md` is
   generated through the refdocs registry
   ([ADR-0069](./0069-generated-reference-docs.md)). Later surfaces, such
   as a per-resource method listing and MCP tool annotations, intersect this
   table with the negotiated features and a resource's kind. They add no
   second description. A method no row admits by verb reports itself as
   mutating, so a read-only hint can never be derived from a denial.
5. Invocation stays typed. There is no `resource.invoke(method, args)` on the
   wire, now or later. A method name missing from the catalog is a usage
   error before any byte is sent. An unknown tag on the wire is already
   `UnknownEnumValue` / `INVALID_COMMAND`.
6. Authorization happens at dispatch. The choke point consumes `classify_*`
   ([ADR-0098](./0098-workload-proof-and-closed-scope-authority.md),
   [ADR-0116](./0116-workload-auth-is-mtls.md)). Discovery is a client-side
   read of a constant and grants nothing.

## Why

The wire already carries every discovery fact that has to be negotiated:
the feature bits a server serves and the kind of each resource. Everything
else about a method (its verb, its subject, its gate) is a property of the
protocol build, so it belongs in the build. A catalog frame would put a
second description of the protocol on the protocol. That description could
disagree with the codec, and it would need version negotiation of its own.

Using one table as both the discovery source and the classifier source
removes a class of bug instead of testing for it. A CLI cannot report a
method as read-only while dispatch requires `BIND`, because both read the
same row. Exhaustive matches make "forgot to classify" a compile error.
The golden (case, requirement, and subject per row) together with the
per-variant samples (which row each message lands on) makes "classified
differently from the spec" a test failure rather than a review catch.

## Tradeoffs

- A new frame or command also touches `kinds.rs` and its sample list, and a
  new §6 row touches both the spec and the table. That friction is
  intended, but it is still friction.
- The table describes this build. A client learns what an older or newer
  server serves only from feature bits and `ResourceKind`. The catalog
  cannot describe methods the client was not compiled with.
- Row case strings are copied verbatim from the spec so the golden can
  compare them, and the test maps each spec subject phrase to a `Subject`.
  Rewording a §6 row is a two-file edit.
- A command that ships without a §6 row is denied by the classifier until
  the spec classifies it. That is the rule working, but once dispatch
  enforces the table a spec gap shows up as refused behavior.

## Alternatives

**A catalog frame (`LIST_METHODS`).** Runtime discovery over the wire.
Rejected: it repeats what the codec already fixes, needs versioning of its
own, and a relay or older peer could answer it in a way that contradicts
what it actually decodes.

**A generic invoke verb with a string method name.** The prototype's
shape. Rejected: it swaps a typed enum for an open namespace that needs its
own authorization model, which is the ambient-authority trap PHA-406 exists
to avoid.

**Keep the classification in the spec only.** Rejected: the dispatch guard
needs a Rust table, and discovery would have to hand-copy verbs. That copy
is the drift this ADR closes.

**Per-tool security flags in MCP.** Today three tools carry an ad hoc
`confirm: true`. Rejected as the model: annotations are derived from this
table instead.
