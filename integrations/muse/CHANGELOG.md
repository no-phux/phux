# Changelog

## 0.3.0 — 2026-10-18

Architectural decoupling: ergonomics are no longer Muse-coupled.

- New `.agents/lib/phux-agent.sh`: harness-agnostic shell library
  (`phux_init`, `phux_spawn`, `phux_run`, `phux_snap`, `phux_wait`,
  `phux_ls`, `phux_kill`, `phux_do`). Any agent or script can source it.
  Documents why `status` keeps its `no_server` contract (read-only probe)
  instead of auto-starting.
- `integrations/muse/` is now a thin consumer: SKILL.md, setup.sh,
  smoke.sh, tests, docs. Removed `scripts/muse-lib.sh`.
- Verified live: setup, smoke, and `phux_do` one-shot all pass.

Ergonomics overhaul from a self-eval drive:

- New `scripts/muse-lib.sh`: sourced shell library (`muse_init`,
  `mspawn`, `mrun`, `msnap`, `mwait`, `mls`, `mkill`, `mdo`) replacing
  raw CLI incantations. `muse_init` auto-starts the server when needed.
- `setup.sh` / `smoke.sh` rewritten on the library (dogfooded).
- All JSON parsed with `jq`; verified CLI forms encoded
  (`phux new --json -s NAME --ephemeral`, `phux kill --yes`).
- `SKILL.md` rewritten around the library with a raw-CLI reference.

## 0.1.0 — 2026-10-18

Initial Muse integration: operating contract (`SKILL.md`), environment
check (`scripts/setup.sh`), end-to-end drive test (`scripts/smoke.sh`),
and structural gates. Covers CLI-driven use from a headless Muse
environment; the MCP path is documented for MCP-capable hosts.
