---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-09
---
# Agent attention evidence implementation

**TL;DR.** Extend the native AgentSession registry with generation-bound,
bounded latest evidence. Keep membership authoritative in the Phux resource
catalog and reuse the existing fenced navigation/inspection seam. TypeScript
receives display projections; terminal input and producer state remain native.

## Data and ordering

The registry owns copied provider/native identity and latest state-bearing
record metadata: sequence, coordinator-stamped timestamp, record type, and a bounded reason
derived from known fields (ask question, notification message, state reason).
Unknown narration neither clears nor replaces state evidence. Missing fields
remain unknown; producer text is presentation data, never a command.

Each registry session tracks the FFI delivery generation. Retained bootstrap
replaces prior evidence for that generation instead of folding over old state.
Live records apply only to the current generation and in increasing sequence
when sequence is supplied. Older generations, including closed delivery, cannot
retire newer membership. Catalog adoption preserves evidence only for the same
resource generation. Validate a complete batch before publishing its fold so
malformed trailing data cannot partially update a row.

Use protocol-backed limits for retained bootstrap versus a single live record;
retained history can be up to the server's bounded backlog, not the single-record
64 KiB limit. The registry retains only latest state-bearing evidence, not the
full payload. Reason truncation is UTF-8 safe and explicit in the projection.

## Projection and action

The existing agent inspection request on `cockpit.navigation` carries paging
and uses the existing snapshot revision fence. Native projection maps each row
to its exact parent navigation entry, walking all split leaves. Extend its
evidence display with latest metadata; no positional durable identity or native
resource handles cross into TypeScript. Snapshot payload remains bounded with
explicit overflow and an independently paged inspection path.

Parent jump uses the existing navigation intent. It changes focus, not agent
state. Only catalog/stream delivery changes membership and evidence. The live
producer fixture must wait for terminal consumption before emitting the next
record, proving that the UI action reached the actual execution surface.

## Validation

Run provider registry and host regressions, compiled shipping markup tests,
the Phux-inclusive Cockpit gate, and the live workflow in
[PRODUCT.md](PRODUCT.md). Record historical behavioral RED for corrected
defects and measure touched function complexity before and after. Independent
review must examine generation replacement, sequence ordering, malformed-batch
atomicity, truncation, and exact-parent fencing before shipment.
