# Changelog

## 0.3.0 — 2026-10-18

Initial CLI guide and checkout-local scripts for headless agents.

- One harness-neutral `.agents/lib/phux-agent.sh`, with no Muse aliases or
  duplicate implementation. Spawn returns an exact local pane selector.
- Scoped variables and cleanup traps; preserved CLI failures and command
  output/exit status; positive finite timeouts and self-target protection.
- Read-only readiness checks; server auto-start belongs only to CLI creation.
- Explicit isolated-socket smoke test and executable script modes.
- Structural and behavioral tests run in the integrations CI lane.
