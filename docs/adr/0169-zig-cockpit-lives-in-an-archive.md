---
audience: contributors, agents
stability: stable
last-reviewed: 2026-10-14
---

# 0169 — Zig Cockpit lives in an archive, not main

**TL;DR.** Remove the tabled Zig Cockpit and its exclusive integration
hooks from main. Keep a published recovery branch and annotated snapshot
tag, with instructions to restore the app and its hooks together.

Status: Accepted
Date: 2026-10-14
Supersedes: [0167](./0167-cockpit-is-frozen.md), [0138](./0138-cockpit-rides-the-next-channel.md)

## Context

The GPUI desktop is the maintained native client. Freezing Zig Cockpit in
the tree stopped automatic builds but retained an unused app, installers,
CLI integration, release workflows, and their contract tests. The user has
chosen not to keep betting on that client, while retaining an easy bot-led
recovery path.

## Decision

- Publish `archive/zig-cockpit` and annotated tag
  `archive/zig-cockpit-2026-10-14` at pre-removal commit `2d267e7a9`.
- Remove `clients/cockpit`, its exclusive fixture generators, Zig mutation
  pilot, build/CI/release hooks, installers, and CLI launch/log/update paths.
- Remove active site promotion and replace its consumer guide with a
  [recovery guide](../consumers/cockpit.md).
- Keep GPUI desktop, shared Rust runtime and C ABI/UniFFI bindings, mobile
  artifacts, and the Zig toolchain required by libghostty.
- Keep historical decisions and research, linking removed sources to the
  immutable archive rather than leaving broken local links.
- Do not delete published releases or modify installed copies.

## Consequences

Main no longer maintains or ships Cockpit. Retired Cockpit tags do not
enter another release lane. A revival starts from current main and reverts
the dedicated removal commit, then ports shared contracts before enabling
automation; merely merging its ancestor archive branch cannot undo deletion.

## Alternatives

- Keep the in-tree freeze: preserves dormant integration burden on main.
- Delete without a named archive: makes recovery unnecessarily archaeological.
- Remove shared FFI or all Zig tooling: breaks maintained native/mobile clients
  and the libghostty engine, which is outside this decision.
