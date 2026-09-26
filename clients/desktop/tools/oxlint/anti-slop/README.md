# Selected anti-slop rules

Vendored from <https://github.com/dmmulroy/anti-slop> at
`c44ef22ca116d0ba62a3ff663a0bd13a3f3fa40b` (reviewed 2026-09-23).
There is no official npm package. The selected rule implementations, their
RuleTester suites, `shared/scope.ts`, and license texts originate from that revision.
`index.ts` is a local, deliberately smaller entrypoint.

Local patch (2026-09-23): strip multiline block-comment decoration before
checking a SAFETY justification. An empty `SAFETY:` block followed only by
asterisks must not authorize a cast. Its regression case extends the original
RuleTester suite; the other implementations and license texts remain unchanged.

| Rule | Reason |
| --- | --- |
| `no-chained-type-assertions` | Reject erased type evidence such as `as unknown as T`. |
| `require-safety-comment-for-type-assertion` | Require a checked invariant for non-const assertions. Comments are review evidence, not runtime validation. |
| `no-module-mocking` | Keep Jest/Vitest tests attached to real dependency seams. Bun mock imports are separately restricted by the desktop config. |

These are syntax/scope rules, not cross-file type proofs. Oxlint's native
type-aware unsafe-operation and promise rules provide separate checks. Unknown
inputs, boundary parsers, runtime `typeof` validation and schema results remain
valid. The upstream blanket unknown/type-shape/naming/spacing policies and
Effect-specific rules are not selected.

`LICENSE` is upstream's MIT notice. Its nested third-party notice is retained at
`vendor/eslint-stylistic/LICENSE`; none of that stylistic implementation is used.

Run `bun run test:rules` from `clients/desktop`. It invokes the vendored suites
with **Node 24**: Oxlint 1.85.0's RuleTester raw-transfer parser does not support
Bun. `@oxlint/plugins` and `oxlint` are pinned together at 1.85.0, compared with
upstream's original 1.78.0. The runtime fixture suite separately exercises these
rules through the actual Oxlint CLI configuration.

On an update, compare these files against this exact revision, retain the local
entrypoint/policy, rerun both suites, and record the new provenance here. Never
copy the full upstream preset implicitly.
