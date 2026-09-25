# phux desktop

## Native framework verification

From the repository root:

```sh
just doctor desktop
just desktop-install
just desktop-source-build
just desktop-check
just desktop-framework-check
```

The framework check compares generated native declarations and Solid artifacts
against the installed packages, builds a production JSX bundle, and drives its
real native window through click and text-input events. It uses the matched
source-built addon, a background-focus window and bounded process cleanup.
The GPU capture is `dist/framework/window.png` under this package.
This fixture verifies the framework, not a working phux terminal application.
The required CI workflow gate runs `desktop-install` and `desktop-check` on
every change. GPU/window checks remain a separate macOS qualification step.

## TypeScript tooling

From `clients/desktop`, with repository-pinned Bun 1.4.0 and Node 24 on `PATH`:

```sh
export BUN_INSTALL_CACHE_DIR="$PWD/.cache/bun"
bun install --frozen-lockfile
bun run check:tooling
```

`check:tooling` runs formatting, strict native TS7, type-aware warning-free
Oxlint, the vendored rule suites and adversarial CLI/JSX fixtures. Individual
commands are `bun run format:check`, `bun run typecheck`, `bun run lint`,
`bun run test:rules` and `bun run test:tooling`. For fixes, run `bun run format`
and `bun run lint --fix`, then repeat the full check. No global JS tool install
or `bunx` download is involved.

### Verified dependency contract

| Purpose                     | Exact package                                 |
| --------------------------- | --------------------------------------------- |
| Native TypeScript compiler  | `typescript@7.0.2` (binary: `tsc`)            |
| Type-aware lint             | `oxlint@1.85.0`, `oxlint-tsgolint@7.0.2002`   |
| JS rule API                 | `@oxlint/plugins@1.85.0`                      |
| Formatting                  | `oxfmt@0.70.0`                                |
| Reactive correctness        | `eslint-plugin-solid@0.18.0`                  |
| Solid runtime               | `solid-js@1.9.15`                             |
| Native renderer / JSX types | `@gpuix/solid@0.10.0`, `@gpuix/native@0.10.0` |
| Bun types                   | `@types/bun@1.4.0`                            |

All versions were verified against the registry on 2026-09-23. GPUIX Solid and
native 0.10.0 were published that day, after earlier research found Solid
unpublished and native at 0.9.0. The lockfile pins the published matching pair;
the native toolchain lane must independently prove its custom native build and
generated declarations against the pinned GPUIX source. A tooling pass is not
that native/GPU evidence.

TS7's `tsc` launcher executes the native platform compiler, not the historical
JavaScript compiler or the older `tsgo` preview. `skipLibCheck` skips checking
third-party declaration implementations, not application uses of those types:
the negative native-event fixture must produce TS2339. Native host JSX uses
`@gpuix/solid`; no ambient substitute types or DOM tag augmentation is present.
TS7 preserves JSX and emits nothing. The smoke test separately calls GPUIX's
Solid universal Bun/Babel plugin to prove JSX compilation.

Oxlint's config is explicitly selected with `-c oxlint.config.ts`; its
`--type-aware` flag requires the installed `oxlint-tsgolint` executable. The
Solid JS plugin recognizes `@gpuix/solid` through `moduleSources`, with selected
reactivity rules rather than a DOM preset. Native JSX component functions use
an explicit `JSX.Element` return type: the pinned type-aware linter otherwise
reports an inferred JSX return as an error type even when TS7 accepts it.

The Solid plugin pulls in ESLint/TypeScript-ESLint support packages whose peer
ranges do not yet include TS7; Bun reports that peer mismatch. The exercised
Oxlint JS-plugin path and Node RuleTester suites pass with these exact versions;
there is no separate ESLint parser or JS `tsc` gate. RuleTester runs under Node
24 because its native raw-transfer parser rejects Bun; the integration suite
and JSX build run under Bun 1.4.0.

### Boundaries enforced by the gate

- Type-aware unsafe operations, floating and misused promises fail.
- Chained casts, unjustified assertions and Jest/Vitest module mocks fail via
  the [selected anti-slop rules](tools/oxlint/anti-slop/README.md).
- Bun mock imports, DOM renderer imports and sibling-client implementation
  imports fail. UI source cannot import Node builtins. The bridge/service
  boundary allows only `node:path`, `node:module`, and `node:fs/promises` for
  native loading and local file operations; direct network imports still fail.
  Transport and reconnect authority belongs to Rust.
- Lost Solid reactivity, destructured props and unused/invalid suppressions fail.
- Native host tags/events, Bun file types and honest `unknown` parsing pass.
- Negative fixtures are excluded from ordinary lint/typecheck and executed
  explicitly by the adversarial suite; expected rule diagnostics are asserted,
  so an unloaded plugin or skipped type-aware check cannot appear green.

Solid owns reactive views. Introduce Effect only when a real non-Solid JS async
service, scoped resource or schema boundary needs it, exact-pinning **Effect v4**
and matching ecosystem packages then. Verify API/runtime/cancellation behavior
on that installed release. This tooling slice contains no such service and adds
no speculative Effect module or unused runtime dependency.
