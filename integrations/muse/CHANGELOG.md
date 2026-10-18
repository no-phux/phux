# Changelog

## 0.2.0 — 2026-10-18

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
