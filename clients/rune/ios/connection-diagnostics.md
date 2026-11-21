# Qualification connection diagnostics

Simulator qualification retains `rune-connection-diagnostics.jsonl` and
`resume-rune-connection-diagnostics.jsonl`. Its opt-in subscriber records only
connection/attempt numbers, monotonic elapsed milliseconds and fixed phase/end
labels. It records no host names, endpoints, identities, raw errors, frames,
terminal text or clipboard data. Normal and device launches do not enable it.
The files distinguish dialing, HELLO, inventory and deadline events even when
the main thread is slow.

Enrollment now owns an explicit trace identity before its blocking runtime is
created; it does not depend on a consumer's task-local scope. The same five-field
rows record fixed `enroll_*` phases for validation, planning, CSR preparation,
direct/relayed connection, request write, reply read, close drain, decoding and
chain validation. Typed outer deadline, refusal, authority change, unreachable,
stalled and exchange-failure labels are emitted before user-facing sanitization.
The last phase identifies the interrupted boundary, not its underlying cause.
No native key bytes or signing payloads are recorded; Swift key creation before
the Rust call is outside this trace.

Each enrollment emits a finite, one-shot sequence; the streaming capture itself
is not a hard byte-limited buffer. Enrollment uses the existing monotonic
identity allocator, avoiding collisions with later consumer attempts. An
enrollment-only trace can describe a failed run but cannot satisfy inventory
readiness, and later inventory cannot erase an enrollment failure.

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

Preparation can fail before any terminal surface or consumer bridge exists.
Only that awaited preparation-failure branch latches the original error, calls
synchronous capture finalization, then publishes its failure report with
`preparation_failed: true` **after finalization returns**. The copied report is
the post-seal acknowledgement for this branch. With that exact report and a
completion receipt, teardown does not request a competing background seal.
This claims no rendered frames or OS lifecycle success; successful consumer
runs still require the normal background acknowledgement. Missing, pending,
failed or mismatched completion and empty traces remain invalid.

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

The same isolated fixture explicitly enables `phux_server::enrollment_diagnostics`.
Field-free root events carry only a local enrollment number, monotonic elapsed
milliseconds and a closed phase enum: request received, work queued/started,
issued/refused inside the blocking worker, async work joined/task failed, then
delivery acknowledged/peer stopped/failed/deadline. The worker-completion and
join boundaries distinguish work duration from delayed runtime resumption.
Delivery acknowledgement is not proof the phone validated or saved the chain.
These debug events change neither the enrollment exchange nor its budgets.

See [Build and qualify](./README.md#build-and-qualify) for the owned fixture.
