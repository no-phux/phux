# Qualification connection diagnostics

Simulator qualification retains `rune-connection-diagnostics.jsonl` and
`resume-rune-connection-diagnostics.jsonl`. Its opt-in subscriber records only
connection/attempt numbers, monotonic elapsed milliseconds and fixed phase/end
labels. It records no host names, endpoints, identities, raw errors, frames,
terminal text or clipboard data. Normal and device launches do not enable it.
The files distinguish dialing, HELLO, inventory and deadline events even when
the main thread is slow.

## Seal and retain

The initial file is copied after the owned app is terminated for saved-identity
relaunch; the resume/failure file is copied after owned simulator shutdown,
before deletion. Immediately before termination, the collector requests one
final OS background transition. The fixture's native callback seals the trace
writer and atomically publishes a create-new, owner-only `.status.json` receipt
with its event count and sticky write-failure flag.

Even a whole lost write leaving valid JSONL, or a lost final write with no later
event, cannot certify capture. A missing, failed or mismatched completion receipt
invalidates preservation. The collector also waits for the existing post-seal
native background acknowledgement before terminating the process. Incomplete
`.status.pending` bytes are retained as invalid evidence, never promoted into a
completed receipt.

Copies never overwrite earlier evidence. `connection-diagnostics.json` records
each copy's SHA-256, strict JSONL/field/phase/timing validation and retention
status. A missing, malformed or nonquiescent trace fails diagnostic preservation
after otherwise successful qualification; an original app failure always remains
the primary error.

## Evidence boundary

The original report remains authoritative: later automatic recovery cannot
replace a failed report or inject its resumed witness. These traces measure
elapsed time, not CPU time or a proven cause of scheduling delays. They change
no connection budgets, retry policy or rendered-input criteria.

The fixture also enables fixed `phux_server::publication_diagnostics` labels in
its existing `server.log`: native capture, initial session/spawn and replacement
bootstrap queues, and publication activation outcomes. An activation refusal,
a gone actor and the existing one-second final-capture deadline are distinct;
the generic pump warning is not itself proof of a timeout. These extra debug
events are off in normal info-level server logging and contain no identities
or payloads.

See [Build and qualify](./README.md#build-and-qualify) for the owned fixture.
