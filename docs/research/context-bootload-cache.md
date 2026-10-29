# Where to cache an agent's context bootload

Research for Beads `phux-hhb7z`, 23 October 2026. Repository examined:
`ac3870eb6beee8795da0540380a6b966ced57c83`. This is a research result,
not an implemented cache or a new provider contract.

## Result

**Keep inference caching at the provider. Keep instruction resolution and any
future prepared-context cache in the harness. Do not add a keeper bootload
service yet.** These are different caches, not interchangeable placements of
one cache: a keeper can cache bytes, a harness can cache preparation, and a
provider can reuse model prefix computation.

The measured first requests contained **14,976 input tokens for OpenCode**
and **11,445 for pi**, with first tool calls at **3.137 s** and **10.415 s**
respectively. Both reported zero first-request cache reads. These are two
single observations with different harnesses and model settings, not a speed
ranking or a measured cache improvement.

The smallest first step is a **harness-side bootload manifest and paired
measurement**, not a shared cache: record what is already in the first request,
its content hashes, preparation time, and provider usage. Test a worker that
uses that already-loaded context instead of being instructed to reread it.
Only then cache preparation if its measured cost justifies it. Keep volatile
fleet observations and mail out of that stable material.

## Measurements

| Fresh worker | First-request input tokens | First tool call |
| --- | ---: | ---: |
| OpenCode v2.1.3, `openai/gpt-6.1-sol` | 14,976 | 3.137 s |
| pi 1.4.0, `openai-codex/gpt-6.1-sol` | 11,445 | 10.415 s |

Cold means a fresh process/conversation, not a flushed OS or provider cache.
The prompt deliberately requested rereads; fleet access was unavailable through
an absent scratch socket. Different settings and extension overhead preclude a
speed comparison. See the [measurement appendix](./context-bootload-cache-measurements.md)
for the full table, methodology, limitations, exact prompt/config and receipts.

## What the repository actually owns today

### Static instructions versus fleet observations

The [historical ADR-0067][adr-0067] requires a stable system/tool prefix and
bounded, sequenced, **persistent tail** fleet observations. The shared
[awareness implementation](../../integrations/runtime/src/awareness.ts) still
provides checkpoints, deltas, unchanged-state suppression, recovery, and
compaction checkpoints. Defaults are 8 KiB, 64 sorted panes, eight deltas per
checkpoint and a one-second refresh timeout. Values are explicitly untrusted;
screens, prompts and credentials are excluded.

[Pi's extension](../../integrations/pi/src/extension.ts) calls `awareness.next`
in `before_agent_start` and returns a hidden custom message, not `systemPrompt`.
Tree changes and compaction append checkpoints. Its `awareness.ts` and
`adapter.ts` re-export the shared runtime. Lifecycle, target selection and mail
are separate concerns; none compiles or caches the harness's instruction files.

**OpenCode V2 is different from the historical ADR.** Its
[context hook](../../integrations/opencode-v2/src/index.ts) always inserts the
static parent-pane rule, defaults automatic awareness **off**, and, if enabled,
pushes a full checkpoint into `event.system` on **each model request**. The
[test](../../integrations/opencode-v2/test/plugin.test.ts) named “fleet context
is opt-in and always a full checkpoint” pins that behavior. There is no
persistent delta base there. The [V2 plugin contract][opencode-plugins] confirms
that context-hook changes affect the outgoing call, not persisted history.
`checkpoint()` also advances `seq` on an unchanged fleet, so the opt-in system
block changes even when no pane changes state.

Consequently it is incorrect to claim all current integrations already satisfy
ADR-0067. With V2 awareness disabled there is no automatic fleet-driven prefix
churn; enabling it can invalidate the conversation prefix from the changed
system block onward. A bootload project must not copy this pattern into its
static cache. Moving opt-in V2 fleet context to persisted tail input is separate
runtime follow-up, not silently implemented or declared fixed by this research.
That follow-up is tracked as Beads `phux-34dnd`.

### Keeper, work store, and mail

[phux-work/service.rs](../../crates/phux-work/src/service.rs) owns a local
Unix-socket keeper: one writer lock, one request per connection, requests served
serially, mail waits parked. The socket is derived from the store path and is
mode 0600. [wire.rs](../../crates/phux-work/src/wire.rs) is a private equal-build
exchange with a semantic epoch and a 1 MiB line cap, **not** the terminal wire
or a generic prompt store. Its request enum has objectives, runs, signals, mail
and claims; no bootload operation.

[store.rs](../../crates/phux-work/src/store.rs) and
[schema.rs](../../crates/phux-work/src/schema.rs) own transactional objectives,
operation receipts, events and additive coordination tables. The current schema
marker is 2; the historical ADR's version-1 description is not today's schema.
Beads' Dolt state is a separate source, not a table or revision in this SQLite
store. A keeper cache cannot infer a valid `bd prime` revision from work-store
events.

[ADR-0187][adr-0187] establishes keeper-owned mail/claims, but its old doorbell
description has evolved: [live agent docs](../agents.md#mail-and-path-claims)
and [runtime mail](../../integrations/runtime/src/mail.ts) use `mail wait`,
take/release and harness conversation delivery, never terminal typing.
[Pi](../../integrations/pi/src/mail.ts) sends a `phux.mail` follow-up;
[OpenCode](../../integrations/opencode-v2/src/mail.ts) queues synthetic input
into the visible TUI session. Mail acquisition and delivery are stateful and
**must never be memoized or replayed as bootload**. Nor should a cache restore
Blackbird registration: the live [substrate decision](../substrate.md) keeps
Blackbird frozen and names one home keeper, while explicitly distinguishing
that policy from the presently local-only keeper transport.

### Harness preparation is already a distinct layer

The installed pi 1.4.0 resource loader reads global and ancestor context files
and resolves skills; its system-prompt builder renders structured sections.
Its extension API exposes `before_agent_start.systemPromptOptions` and
`context_with_system`. It recommends section changes instead of whole-prompt
replacement so transcript deltas can preserve prefixes. These observations
come from the installed package's `dist/core/{resource-loader,system-prompt,
agent-session}.js` and `docs/extensions.md`, not an assumed older pi API.

A `before_agent_start` extension sees already-resolved options. Caching a string
there does **not** bypass earlier instruction discovery, extension loading,
trust checks or hooks. Replacing the whole system prompt also risks undoing
later extensions and pi's structured transcript handling. Cross-process resource
reuse, if worthwhile, belongs at the harness resource-loader/compiler boundary,
not as an unconditional replacement from the phux extension.

## Three options

| Placement | Cache and likely benefit | What phux would own | What it cannot solve |
| --- | --- | --- | --- |
| Before inference: keeper-served bootload | Immutable prepared source bundle, reused across workers; potentially removes repeated subprocesses and model-driven discovery turns | Manifest schema, trusted source acquisition, per-worktree scopes, storage/GC, freshness, CLI/keeper exchange, adapters for each harness | No model KV reuse by itself; bytes still consume context and need provider processing on a miss; cannot impose one harness's instruction precedence on another |
| At inference: provider prefix cache | Reuses model computation for identical eligible prefixes; observed within-session hits already exist | Stable phux-owned context layout, deterministic tools, measurements, and nonvolatile integration guidance | Does not eliminate file reads, hook execution or the model generating redundant tool calls; cannot guarantee reuse across models, accounts, workers or providers |
| In harness: compiled prompt/resource cache | Reuses instruction discovery/parsing/rendering for matching content; can supply already-loaded-source provenance | At most a small phux integration consuming the manifest and preserving the tail; the harness should own resolution/compiler correctness | A per-process map cannot benefit a new process; hashing files still requires freshness work; cached rendering does not replace model inference or automatically stop rereads |

These can compose. A harness-prepared bundle should be byte-stable enough for
provider caching whether it came from local files or a future keeper. A keeper
snapshot is **not** a shared provider KV cache. A whole bootload including live
fleet, task ownership and mail cannot honestly be a stable prefix.

Provider documentation fetched 23 October 2026 supports exact-prefix matching,
stable tools/instructions first and changing content later
([OpenAI][openai-cache], [Anthropic][anthropic-cache]). Eligibility, breakpoints,
retention, cache-write charges and account/model boundaries differ. Identical
bytes are necessary but not sufficient: a useful shared boundary must actually
have been written and be eligible for lookup. Do not bake one vendor's TTL or
discount into phux. A local content digest is not a provider routing key and is
not a security credential. API documentation is not proof that a subscription
transport or these installed adapters exposes every documented cache control.

## Cache key and invalidation design

If a prepared cache is justified, split it into **static**, **Beads-derived**,
and **live** pieces. The complete bundle can name both static and Beads hashes,
but only the static piece belongs at the invariant front of model context.

```text
static_key = SHA256(canonical_manifest(
  format_version, harness_and_compiler_version, effective_trust_and_policy,
  repo_and_worktree_scope, cwd_and_resolution_rules,
  ordered_instruction_paths_and_content_hashes,
  effective_skill_catalog_and_selected_skill_content_hashes,
  hook_definitions_and_extension_versions, effective_tool_schema_hash
))
bd_key = SHA256(canonical_manifest(
  bd_version, resolved_bd_store_identity, effective_profile_and_config,
  prime_output_hash, relevant_task_snapshot_hash
))
bundle_key = SHA256(static_key, bd_key)
```

“Canonical” means a versioned, length-delimited encoding, stable object-key
ordering and preserved **semantic** list order. Never alphabetize instruction
precedence or active tools merely to create a hit. Include missing-file markers,
resolved symlink targets and discovery-directory membership so additions and
deletions invalidate too. Include global/user/ancestor rules and referenced
instruction files, not just the two repository-root filenames. `CLAUDE.md`'s
`@AGENTS.md` points to another dependency; identical text is not permission to
drop a harness's include semantics.

If caching the fully compiled prompt rather than source fragments, also key
the effective agent/mode, ordered extension contributions, selected tool loadout,
model-specific prompt variant and serializer version. Verify the hash of the
actual rendered sections. Identical instruction files alone do not imply an
identical request, and provider/model eligibility remains a separate cache scope.

The skill dependency includes effective ids, metadata, order, paths, package
versions, visibility/trust decisions and content digests. Cache the **catalog**
plus only selected bodies, retaining lazy skill loading; do not preload every
skill into every worker. Hook *definitions* may be hashed; hook *executions*
must still run where their contracts require them. Hooks can depend on time,
identity, credentials or side effects and are not pure file transforms.

| Change or failure | Required behavior |
| --- | --- |
| Instruction edit, same-size edit, dirty worktree, include edit | Content hash changes; rebuild the affected static piece. Git HEAD or mtime alone is insufficient. |
| New/deleted rule, skill, plugin, nested cwd, symlink retarget, changed trust | Rediscover effective membership and precedence before accepting a hit. Do not cross worktree or trust scopes. |
| Harness/compiler/tool schema/hook settings change | New static key; no reuse of another harness's compiled prompt. |
| `bd remember`, profile/config change, task claim/status/dependency change, store sync | Refresh the relevant Beads piece. Use authoritative `bd` output or a supported revision API; never treat `.beads/issues.jsonl` as live truth or use phux SQLite revision as a substitute. |
| Beads cannot provide a cheap reliable revision | Run `bd prime`/the required read and hash the output; claim no subprocess saving until a reliable freshness mechanism exists. |
| Fleet change, attention change, mail, claims | Fresh bounded tail observation or normal delivery. No static-key invalidation and no reuse of a previously taken mail delivery. |
| Compaction, branch navigation, resume | Revalidate static scope; restore the harness transcript and a fresh fleet checkpoint. Never restore another session's deltas as the base. |
| Hash/read race, unsupported manifest, corrupt blob, expired dynamic data | Treat as a miss and load current sources. A cache failure must not silently omit mandatory rules or label stale state current. |

Validate a consistent read-set before publishing: discover, read/hash, render,
then check that membership and source versions still match; retry a bounded
number of times or fall back to the ordinary loader. Watchers are invalidation
hints, not the source of truth. Store immutable content-addressed blobs with
atomic publication and bounded retention, separate from durable task history.
Use single-flight preparation per key if concurrent cold workers justify it;
never run unbounded file walks or `bd prime` inside the serial keeper request
loop, where they would delay mail and claims.

Put `bd_key`, task ownership, timestamps and fleet identity in local provenance
or later context, **not before the reusable static prefix**. A single hash of
“instruction files + skills + all bd state + fleet” at the front would recreate
the exact invalidation problem ADR-0067 avoids. Beads' stable workflow guidance
may eventually be separated from current task data, but only through a supported
source contract, not heuristic stripping of lines from `bd prime`.

## ACP interaction

[ADR-0170][adr-0170] and the [live ACP guide](../acp.md) put provider ownership
in the installed ACP agent/adapter. Phux supervises the process and activation,
permissions and durable operations; it does not own the vendor request builder.
The actual [ACP host](../../crates/phux-agent-host/src/session/acp.rs) initializes
default client capabilities, creates or loads a session with cwd, then sends
`PromptRequest` containing text. It does not send a compiled system prompt,
configure prefix-cache breakpoints or advertise filesystem/terminal callbacks.

Therefore ACP is not a universal “inject stable system prefix” hook. Sending a
bootload as ordinary prompt text consumes tokens, changes its instruction role,
and may duplicate what the ACP agent already loaded from cwd. Even if a future
agent supports resource blocks or configuration extensions, negotiate and test
that capability rather than treating it as the pinned baseline. Keep the
official ACP seam; do not resurrect provider SDK bridges to implement caching.
Terminal pi/OpenCode plugins remain a separate, supported harness-side path.

A keeper option would initially need an explicit harness consumer; ACP could
only carry a bundle reference/text with honest ordinary-context semantics, or
an explicitly supported adapter extension. A remote home keeper cannot read
another host's local dirty worktree: acquisition and validation must happen on
the worker host within its own authority. Existing keeper policy is not
authorization to upload private instruction files across hosts.

## Risks and recommendation boundary

- **Staleness changes behavior, not just performance.** Never reuse revoked
  trust, permission answers, path-claim fences, credentials or task ownership.
  A hash identifies bytes, not their authority or freshness.
- **Prompt injection and privilege elevation.** Fleet names/cwds, mail and
  tracker text remain attributed data. Putting them in a keeper-issued system
  block would give attacker-controlled bytes more authority, not sanitize them.
- **Private context retention.** Instruction and skill files can contain local
  paths and sensitive material. Cache under the same principal and trust scope
  with private permissions, bounded retention, no raw diagnostic upload, and
  no implicit cross-host sharing. A digest can still reveal equality.
- **Double loading and precedence drift.** A phux snapshot plus the harness's
  native AGENTS discovery can repeat or reorder rules. A matching AGENTS hash
  alone does not justify bypassing the rest of the harness compiler.
- **False performance attribution.** These timings include model/transport and
  extension work. They do not isolate prompt rendering, prove cross-worker
  provider hits, establish p50/p95, or quantify a keeper cache's savings.

**Recommendation: provider prefix reuse, with harness-owned preparation; no new
keeper API, schema or daemon work for now.** The code already has a fleet
projection and the harnesses already load instructions. There is no measurement
here showing that reconstructing the compiled system string is the bottleneck.
The forced reread workflow adds model/tool turns that none of the three caches
can remove without changing how bootload is supplied and acknowledged.

The first follow-up should be one opt-in, read-only harness manifest/trace:
record ordered loaded-source hashes and “already included” provenance out of
band, first-request ordinary/read/write token counters, source preparation time,
and first-tool latency. A/B fresh workers with the same model, tools and policy,
with versus without the redundant reread instruction; include a paired
unchanged-prefix request, one rule edit, one skill edit and a Beads-state edit.
Use an isolated empty/fake fleet, then a changed-fleet fixture to verify the
prefix survives. Do not put run ids or manifest timestamps at the request front.

Keep OpenCode V2 awareness off until its opt-in path has a tested persistent-tail
design; a focused follow-up can align it with the still-useful ADR-0067
constraint without coupling that fix to a cache rollout. If later measurements
show preparation dominates, pilot a content-keyed **resource cache inside one
harness**, retaining native trust and structured sections. Consider keeper
sharing only after multiple harnesses demonstrate the same expensive pure
input preparation, with worker-local freshness and an actual supported consumer
seam. That is a higher evidence bar than “every worker reads files.”

[adr-0067]: https://github.com/no-phux/phux/blob/d21db2e345aeaff43a681b3a04dfd43f2cc082ca/docs/adr/0067-cache-preserving-agent-fleet-context.md
[adr-0170]: https://github.com/no-phux/phux/blob/d21db2e345aeaff43a681b3a04dfd43f2cc082ca/docs/adr/0170-acp-is-the-provider-seam.md
[adr-0187]: https://github.com/no-phux/phux/blob/d21db2e345aeaff43a681b3a04dfd43f2cc082ca/docs/adr/0187-agents-coordinate-through-phux.md
[opencode-plugins]: https://opencode.ai/v2/docs/build/plugins
[openai-cache]: https://developers.openai.com/api/docs/guides/prompt-caching
[anthropic-cache]: https://platform.claude.com/docs/en/build-with-claude/prompt-caching
