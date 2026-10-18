---
audience: contributors
stability: stable
last-reviewed: 2026-10-17
---

# 0183 — The client-neutral config is its own crate

**TL;DR.** `crates/phux-config-core` holds the client-neutral half of
`config.toml`: its path, the `extends` merge, `[[remote]]`, `[organize]`,
and the instance, socket and known-authority paths. `phux-config` keeps
the TUI's schema, `default.toml` and widgets, and re-exports the core.
`phux-client-runtime` depends on the core only, so no embedder builds the
TUI's config. CI routing reads the same edges.

Status: Accepted
Date: 2026-10-17

## Context

`phux-client-runtime` (ADR-0133) read the remote registry and `[organize]`
through `phux_config::loader::load_from`, which merges the shipped
`default.toml` and deserializes the whole schema: keybindings, the status
bar, widgets, themes. So the desktop host built `phux-config`, and a change
to the TUI's defaults rebuilt and retested the desktop on two macOS
runners. It also meant a typo in `[keybindings]` broke the desktop's remote
resolution, which never reads keybindings.

## Decision

1. **`phux-config-core` owns the client-neutral tables.** It has the config
   path, the ADR-0039 layer merge with the defaults supplied by the caller,
   `ConfigError`, `RemoteConfigEntry`, `OrganizeCfg`, and the `instance`,
   `socket` and `known_authorities` modules. `loader::load_client_from`
   returns a `ClientConfig` of `[[remote]]` and `[organize]`. It ignores
   other tables, and the two it reads keep their unknown-field checks.
2. **`phux-config` re-exports the core** under its existing paths and passes
   `default.toml` to the merge, so its callers do not change. A test pins
   that `default.toml` sets no table `ClientConfig` reads, because the
   client view merges no defaults.
3. **Embedders depend on the core only.** `phux-client-runtime` names
   `phux-config-core`. `phux-client-ffi`'s `c-abi` registry keeps
   `phux-config`, behind a feature the desktop does not enable.
4. **CI routes by resolved edges.** `scripts/ci/classify-changes.py` walks
   each lane's manifests the way Cargo resolves them, and follows an
   optional dependency only when an enabled feature turns it on. A
   `phux-config` change no longer requests the desktop lane.

## Why

The desktop's graph should be what it compiles. Routing by crate is exact
only when crates are cut at real boundaries. A module-level route table
would be one more list to keep in step. The table the desktop reads is a
small, stable part of the file, so it can live below the TUI's schema
without that schema leaking in.

## Tradeoffs

There is one more crate. `phux-config-core`'s layer merge is the TUI's
merge, so a layering change still reaches every lane, which is correct.
The client view only checks the tables it reads. A broken `[status]`
table still fails `phux config check` and the TUI, but no longer the
desktop's dial.

## Alternatives

**Route by module in the classifier.** Rejected: this is a path list that
does not follow the code. It would fail open when a new import crossed the
line.

**Move the TUI's schema into `phux-tui`.** This would be a cleaner end
state, but `phux`, `phux-server`, `phux-mcp` and `phux-plugin` read
`Config` too, and the settings catalogue, `check` and `scaffold` span both
halves. The client-neutral cut gives the desktop its boundary now, and
does not rule that move out later.
