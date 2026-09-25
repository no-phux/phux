---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-23
---

# Solid native-element admission patch

**TL;DR.** Patch 3 adds explicit, framework-neutral custom-element registration
and lets Solid mount those tags through its normal reactive host path. Initialized
macOS renderers expose a non-instantiating factory query, so missing factories
fail clearly. The patch is verified with real Metal painting, events, reactive
props, and balanced unmount/remount of the existing native probe.

## Provenance and scope

Apply after patches 1 and 2 against GPUIX
`6d5e6887ad8dc6e94eb66043394f3d17a462c56a` / Zed
`81c99f816b4a5f69d3c014774068034c24d1d7af`.

- Patch 2 remains byte-for-byte pinned at
  `d346c3923e556dd135158f0998363811f8a8311ae2262d3518dab070062dbaf4`.
- Patch 3 SHA-256:
  `9dd437ce3533243eb35047cdbd404bcc50003463ecf3fb85a51dda17b98e166d`.
- Development/build used this lane's original patch-1 baseline. Read-only
  `git apply --check` also passed against the parent's current source, whose
  patch-1 hash is `42d56bb564ea8e87d0c0a872a918c05d597881fd0a1cd69a04287ad9aa69132d`.
- No dependencies, manifests, locks, loader, bootstrap, host Rust or earlier
  patch artifacts changed. No publication or Beads updates.

Implementation touches five source/declaration files: shared `native/js/host.ts`,
Solid `src/host.ts`, native registry/renderer, and generated `index.d.ts`.
The patch also carries shared JS tests, upstream README documentation and a
changeset. The existing React adapter still typechecks against the widened
`ElementType` union.

## Exports and loader wiring

From `@gpuix/native/host`:

- `registerCustomElementType(name: string): CustomElementType`
- `isHostElementType(name: string): name is ElementType`
- `isCustomElementType(name: string): name is CustomElementType`
- Branded `CustomElementType`; `ElementType` includes built-ins plus that type.

Names must be lowercase ASCII hyphen-separated names. Built-ins cannot be
overridden. Registration is idempotent across reloads and additive for the
process lifetime. It imports no addon and never constructs a native registry,
creates a native instance or seals startup.

The application bootstrap sequence is:

```ts
// Application-owned loader: choose the one addon and install its factories.
const host = loadDesktopHost(absoluteAddonPath)
// Resolve this module and Solid to the same verified, built source packages.
const { registerCustomElementType } = await import('@gpuix/native/host')
registerCustomElementType('phux-terminal')
// Then construct/init renderers and createRoot(renderer).render normal JSX.
```

Application code augments `@gpuix/solid/jsx-runtime`'s
`JSX.IntrinsicElements` with its typed terminal props. No terminal-specific
exception exists in the patch. The upstream README and native fixture contain
working augmentation examples. Rebuild the addon and shared JS/Solid packages;
regenerate the parent's combined declarations after its FFI integration.

`GpuixRenderer.hasCustomElementType(name)` is macOS-only and checks the
initialized window's existing registry. The query creates nothing and rejects
closed/uninitialized windows. Solid checks it before emitting a registered
custom node's creation mutation. Native's existing raw unknown-type behavior
is warning plus `gpui::Empty`; this patch prevents that silent result on the
Solid path when the query exists. Backends without the optional query retain
explicit JS admission but cannot prove native availability.

## Complexity and verification

TypeScript compiler AST counts exclude nested functions and optional-chain
access; Rust's new lookup/query methods are branch-free (CCN 1 each).

| Function | Before | After |
|---|---:|---:|
| Solid `createHostElement` | 2 | 2 |
| Solid private `isElementType` switch | 13 | removed |
| shared `isHostElementType` | new | 2 |
| `registerCustomElementType` | new | 4 |
| `customElementTypes` / `isCustomElementType` | new | 1 / 1 |
| Solid `mount` | 14 | 8 |
| extracted `mountProperties` | new | 7 |
| `validateNativeFactory` | new | 4 |

All touched/new implementation functions are at or below CCN 10. Scope stays
within tag admission, native availability and the directly affected mount path.
Clean ordered application, byte-for-byte developed-source comparison and
reverse-check passed. See the [native receipt](../../native/SOLID-NATIVE-ELEMENTS.md)
for exact release/GPU evidence and checks.
