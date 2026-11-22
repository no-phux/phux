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

## Unvalidated failure files

If qualification or teardown fails, the collector also attempts a separate
`UNVALIDATED-native-failure/` capture after successful shutdown of its exact
owned simulator and before deletion. This recovers files published while a
blocking health query prevented a report waiter from observing them. The same
boundary covers initial/resumed reports, Liquid Glass, OS background/foreground
receipts and final seal acknowledgement; it does not retry any of those steps.

The fixed allowlist is `rune-qualification.json`, `rune-tree.json`,
`rune-input-gate.json`, `rune-glass.json`, `rune-background.json`,
`rune-lifecycle.json`, and the active connection capture's `.status.json` and
`.status.pending`. Native files use the active process's initial or `resume-`
prefix in the forensic directory. Scene files can predate the failed step;
their presence alone does not bind them to that step. Pairing configuration,
preferences, keys and arbitrary container files are never collected.

Failure cleanup's seal handshake replaces `rune-background.json`. Before that
handshake, the collector moves an existing acknowledgement into an exclusive
`UNVALIDATED-before-seal/` directory inside the owned container, without reading
its bytes. After shutdown it retains that file separately as
`rune-background.before-seal.json` (with the active process prefix), so the later
seal acknowledgement cannot erase the original lifecycle receipt. The staging
directory is not an extra source allowlist: only its fixed background file can
be read, with the same safety checks and quiescence requirement.

`receipt.json` labels every copy **UNVALIDATED**, with its original byte length
and SHA-256, or records a missing/rejected file. Malformed reports and absent
trees remain forensic failures, never accepted reports or rendered-input proof.
No JSON is repaired, parsed into replacement evidence or fed back to acceptance.
Copies are exclusive and private; symlinks (including directory ancestors),
hardlinks, nonregular files, foreign-owned files and changing sources are rejected.
The constructor canonicalizes its trusted output path, like the CLI, so macOS
temporary-directory aliases work for direct callers too. Untrusted source paths
are never canonicalized; capture still opens directory components without following
symlinks, including any replacement of the canonical output after construction.
If shutdown does not establish producer quiescence, no native bytes are copied;
the receipt records that limitation and rejects every file.

Each forensic source is limited to **16 MiB**. Larger sources are rejected by
`fstat` before reading; every read is bounded to 16 MiB plus one overflow byte
to detect concurrent growth. Overflow is rejected, never truncated or copied as
a complete capture. Other allowlisted files and owned deletion still proceed.
This is only a forensic retention limit, not a native qualification/report
acceptance limit; it changes no assertion, input rule, deadline or retry.

This capture never overwrites normal retained reports or an earlier forensic
capture. The original host/app exception remains primary even when preservation
fails, and owned deletion still runs. It repairs diagnostic loss, not simulator
responsiveness, connection budgets, qualification criteria or a failed release.

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

## One-off hosted command-stall diagnosis

Dispatch the registered `rune-macos.yml` on the diagnostic branch with
`native_diagnostic=true`; its reusable `rune-native-diagnose.yml` job is **not a
qualification or release gate**. Explicit diagnostic dispatch skips the normal
plan/build, cannot cancel normal lanes, and does not produce a passing
`rune-macos` aggregate or validation receipt. Normal triggers/defaults are unchanged.
It verifies artifact 14755772123's API provenance, immutable size/SHA-256
and safe archive entries, then reuses its original simulator app. The root server
and qualifier must remain source-equivalent to the original run; only release
metadata and the diagnostic files may differ. There is no native rebuild,
distribution signing, phone operation, or production-server access.

`diagnose_commands.py` subclasses only the command runner and records the
owned app's identity. It invokes the unmodified QUIC sequence, then WSS only if
QUIC completes, with the same commands, assertions, retries and budgets. Commands
still running after ten seconds get bounded sampling of recorded owned command,
app and server PIDs, with microsecond start-identity rechecks. Shared services
are never sampled or signalled. Resource snapshots omit process arguments;
CoreSimulator logs are restricted to the owned device. Each diagnostic helper
has a two-second limit and is cancelled/reaped when its command finishes.

Sampling briefly suspends its targets, so even successful output is **diagnostic
counterevidence, not pristine timing or release acceptance**. Receipts are renamed
`DIAGNOSTIC-ONLY-evidence.json`; status files explicitly deny release qualification.
The separate artifact is `diagnostic-ios-command-stalls`. Compare its recorded
host image and simulator runtime with the original: a hosted image can drift,
and a successful run on another image/runtime does not explain the original stall.
