#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARTIFACT_DIR="${PHUX_PARITY_ARTIFACT_DIR:-$ROOT/.omo/evidence}"

SCENARIOS=(
  install-contract
  examples-smoke
  plugin-demo
  real-pty-run-wait
  tui-probe
  visual-qa-hooks
  docs-check
  full-quality-gates
)

usage() {
  cat <<'EOF'
parity-gate.sh - herdr parity QA and evidence harness

USAGE:
  bash scripts/parity-gate.sh --list
  bash scripts/parity-gate.sh --check-list
  bash scripts/parity-gate.sh --run [all|SCENARIO...]
  bash scripts/parity-gate.sh --help

MODES:
  --list        Print the parity scenarios and the real command each wraps.
  --check-list  Verify every named scenario is present and references existing
                just targets, scripts, test files, and example/plugin assets.
  --run         Run the requested scenario(s). With no scenario, runs all.

ENV:
  PHUX_PARITY_ARTIFACT_DIR  Directory for real-surface artifacts
                            (default: .omo/evidence).
EOF
}

scenario_description() {
  case "$1" in
    install-contract)
      echo "install docs/scripts/release artifact contract checker"
      ;;
    examples-smoke)
      echo "real phux binary against examples/agents smoke scripts"
      ;;
    plugin-demo)
      echo "checked-in plugin package discover/validate/run flow"
      ;;
    real-pty-run-wait)
      echo "ignored e2e lane covering real PTY run/wait behavior"
      ;;
    tui-probe)
      echo "bounded assertion-bearing real-server TUI, paging, restore, and cast replay gate"
      ;;
    visual-qa-hooks)
      echo "captured TUI screens, cursors, logs, casts, and replay snapshots"
      ;;
    docs-check)
      echo "doc-system frontmatter/TLDR/link/spec gate"
      ;;
    full-quality-gates)
      echo "full fmt/lint/docs/tests/deny/rustdoc CI gate"
      ;;
    *)
      return 1
      ;;
  esac
}

scenario_command() {
  case "$1" in
    install-contract)
      echo "bash scripts/check-install-surface.sh"
      ;;
    examples-smoke)
      echo "just examples-smoke"
      ;;
    plugin-demo)
      echo "just plugin-demo"
      ;;
    real-pty-run-wait)
      echo "just e2e"
      ;;
    tui-probe)
      echo "bash scripts/tui-probe.sh 80 24"
      ;;
    visual-qa-hooks)
      echo "PHUX_SMOKE_ARTIFACT_DIR=\$PHUX_PARITY_ARTIFACT_DIR bash scripts/tui-probe.sh 100 30 | tee \$PHUX_PARITY_ARTIFACT_DIR/parity-tui-probe.txt && grep -q '=== PASS ===' \$PHUX_PARITY_ARTIFACT_DIR/parity-tui-probe.txt"
      ;;
    docs-check)
      echo "just docs-check"
      ;;
    full-quality-gates)
      echo "just ci"
      ;;
    *)
      return 1
      ;;
  esac
}

list_scenarios() {
  local scenario
  for scenario in "${SCENARIOS[@]}"; do
    printf '%-18s  %s\n' "$scenario" "$(scenario_description "$scenario")"
    printf '%-18s  command: %s\n' "" "$(scenario_command "$scenario")"
  done
}

has_scenario() {
  local wanted="$1"
  local scenario
  for scenario in "${SCENARIOS[@]}"; do
    [[ "$scenario" == "$wanted" ]] && return 0
  done
  return 1
}

require_file() {
  local rel="$1"
  if [[ ! -e "$ROOT/$rel" ]]; then
    echo "missing required path: $rel" >&2
    return 1
  fi
}

require_just_target() {
  local target="$1"
  if ! just --summary --justfile "$ROOT/justfile" --working-directory "$ROOT" \
      | tr ' ' '\n' | grep -qx -- "$target"; then
    echo "missing required just target: $target" >&2
    return 1
  fi
}

check_list() {
  local tmp
  tmp="$(mktemp "${TMPDIR:-/tmp}/phux-parity-list.XXXXXX")"
  list_scenarios >"$tmp"

  local scenario
  for scenario in "${SCENARIOS[@]}"; do
    grep -Eq "^${scenario}[[:space:]]" "$tmp" || {
      echo "scenario missing from --list output: $scenario" >&2
      rm -f "$tmp"
      return 1
    }
  done

  require_file justfile
  require_file scripts/parity-gate.sh
  require_file scripts/check-install-surface.sh
  require_file scripts/examples-smoke.sh
  require_file scripts/tui-probe.sh
  require_file scripts/check-docs.sh
  require_file crates/phux/tests/run_wait_e2e.rs
  require_file examples/agents/01-ls-and-snapshot.sh
  require_file examples/plugins/agent-tools/phux-plugin.toml
  require_file examples/plugins/agent-tools/config/phux/config.toml

  require_just_target examples-smoke
  require_just_target plugin-demo
  require_just_target e2e
  require_just_target docs-check
  require_just_target ci
  require_just_target parity-check-list
  require_just_target parity-gate

  rm -f "$tmp"
  echo "parity-gate check-list passed: ${#SCENARIOS[@]} scenarios reference real repo surfaces"
}

run_scenario() {
  local scenario="$1"
  has_scenario "$scenario" || {
    echo "error: unknown scenario: $scenario" >&2
    return 2
  }

  mkdir -p "$ARTIFACT_DIR"
  echo "== parity scenario: $scenario =="
  echo "command: $(scenario_command "$scenario")"

  case "$scenario" in
    install-contract)
      (cd "$ROOT" && bash scripts/check-install-surface.sh)
      ;;
    examples-smoke)
      (cd "$ROOT" && just examples-smoke)
      ;;
    plugin-demo)
      (cd "$ROOT" && just plugin-demo)
      ;;
    real-pty-run-wait)
      (cd "$ROOT" && just e2e)
      ;;
    tui-probe)
      (cd "$ROOT" && bash scripts/tui-probe.sh 80 24)
      ;;
    visual-qa-hooks)
      local artifact="$ARTIFACT_DIR/parity-tui-probe.txt"
      (cd "$ROOT" && PHUX_SMOKE_ARTIFACT_DIR="$ARTIFACT_DIR" bash scripts/tui-probe.sh 100 30) | tee "$artifact"
      grep -q "=== PASS ===" "$artifact"
      echo "visual QA artifacts: $ARTIFACT_DIR"
      ;;
    docs-check)
      (cd "$ROOT" && just docs-check)
      ;;
    full-quality-gates)
      (cd "$ROOT" && just ci)
      ;;
  esac
}

run_requested() {
  local requested=("$@")
  if [[ "${#requested[@]}" -eq 0 || "${requested[0]}" == "all" ]]; then
    requested=("${SCENARIOS[@]}")
  fi

  check_list

  local scenario
  for scenario in "${requested[@]}"; do
    run_scenario "$scenario"
  done
}

if [[ "$#" -eq 0 ]]; then
  usage >&2
  exit 2
fi

case "$1" in
  --help|-h)
    usage
    ;;
  --list)
    shift
    [[ "$#" -eq 0 ]] || {
      echo "error: --list takes no arguments" >&2
      exit 2
    }
    list_scenarios
    ;;
  --check-list)
    shift
    [[ "$#" -eq 0 ]] || {
      echo "error: --check-list takes no arguments" >&2
      exit 2
    }
    check_list
    ;;
  --run)
    shift
    run_requested "$@"
    ;;
  *)
    echo "error: unknown argument: $1" >&2
    usage >&2
    exit 2
    ;;
esac
