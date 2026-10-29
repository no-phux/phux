#!/usr/bin/env bash
# demo-pi-setup.sh — one command from a checkout to shot 1 of the Pi demo.
#
# Shot 1 is the `work` session in the split recorded by
# docs/assets/pi-live-fleet.cast and examples/agents/record-pi-live-fleet:
#   seed pane
#   vertical split, ratio 0.55 (seed keeps the left side; side-by-side)
#   horizontal split, ratio 0.5, of that new pane (stacked)
# The seed directory is a git repo whose test fails on purpose.
#
# `@phux/pi` and the phux binary are versioned independently. This script
# treats them as matched when the installed `@phux/pi` equals
# integrations/pi/package.json and `phux --version` equals this checkout's
# workspace version.
#
# Usage: scripts/demo-pi-setup.sh
#   PHUX                 binary to use (default: PATH, then target builds)
#   PHUX_PI_PACKAGE      directory of an @phux/pi package (overrides discovery)
#   PHUX_PI_DEMO_DIR     repo to seed (default: ~/phux-pi-demo)
set -euo pipefail
# shellcheck source=scripts/lib/output.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/output.sh"

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SESSION=work
DEMO_DIR="${PHUX_PI_DEMO_DIR:-${HOME:?}/phux-pi-demo}"

die() {
  phux_error "demo-pi-setup: $*"
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || die "missing $1"
}

need python3
need git

expected_phux="$(
  python3 - "$REPO/Cargo.toml" <<'PY'
import pathlib, sys
text = pathlib.Path(sys.argv[1]).read_text()
for line in text.splitlines():
    if line.startswith("version = "):
        print(line.split('"')[1])
        break
else:
    raise SystemExit("workspace version not found")
PY
)"
expected_pi="$(
  python3 - "$REPO/integrations/pi/package.json" <<'PY'
import json, pathlib, sys
print(json.loads(pathlib.Path(sys.argv[1]).read_text())["version"])
PY
)"

resolve_phux() {
  if [[ -n "${PHUX:-}" ]]; then
    [[ -x "$PHUX" ]] || die "PHUX is not executable: $PHUX"
    printf '%s\n' "$PHUX"
    return
  fi
  if command -v phux >/dev/null 2>&1; then
    command -v phux
    return
  fi
  if [[ -x "$REPO/target/release/phux" ]]; then
    printf '%s\n' "$REPO/target/release/phux"
    return
  fi
  if [[ -x "$REPO/target/debug/phux" ]]; then
    printf '%s\n' "$REPO/target/debug/phux"
    return
  fi
  echo "demo-pi-setup: no phux binary; installing the current release" >&2
  "$REPO/scripts/install.sh"
  local installed="${HOME}/.local/bin/phux"
  [[ -x "$installed" ]] || die "install.sh did not produce $installed"
  printf '%s\n' "$installed"
}

phux_version() {
  local raw
  raw="$("$1" --version)"
  python3 -c 'import re,sys; m=re.fullmatch(r"phux\s+v?(\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?)", sys.argv[1].strip());
assert m, sys.argv[1]; print(m.group(1))' "$raw"
}

# Pi loads a global @phux/pi ahead of a project copy. Honor that order.
find_pi_package() {
  if [[ -n "${PHUX_PI_PACKAGE:-}" ]]; then
    printf '%s\n' "$PHUX_PI_PACKAGE"
    return
  fi
  python3 - "$HOME" "$REPO" <<'PY'
import json, pathlib, sys
home, repo = map(pathlib.Path, sys.argv[1:])
global_pkg = home / ".pi/agent/npm/node_modules/@phux/pi"
if (global_pkg / "package.json").is_file():
    print(global_pkg)
    raise SystemExit
candidates = [
    home / ".pi/agent/settings.json",
    repo / ".pi/settings.json",
]
for settings in candidates:
    if not settings.is_file():
        continue
    data = json.loads(settings.read_text())
    for entry in data.get("packages") or []:
        if not isinstance(entry, str) or entry.startswith("npm:"):
            continue
        path = pathlib.Path(entry)
        if not path.is_absolute():
            path = (settings.parent / path).resolve()
        manifest = path / "package.json"
        if not manifest.is_file():
            continue
        if json.loads(manifest.read_text()).get("name") == "@phux/pi":
            print(path)
            raise SystemExit
PY
}

pi_package_version() {
  python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' "$1/package.json"
}

install_pi_package() {
  command -v pi >/dev/null 2>&1 || die "@phux/pi is not installed and pi is not on PATH. Install Pi, then: pi install npm:@phux/pi@${expected_pi}"
  echo "demo-pi-setup: installing @phux/pi@${expected_pi}" >&2
  pi install "npm:@phux/pi@${expected_pi}"
}

PHUX_BIN="$(resolve_phux)"
actual_phux="$(phux_version "$PHUX_BIN")"
[[ "$actual_phux" == "$expected_phux" ]] || die "phux ${actual_phux} does not match this checkout's phux ${expected_phux}"

pi_dir="$(find_pi_package || true)"
if [[ -z "$pi_dir" ]]; then
  install_pi_package
  pi_dir="$(find_pi_package || true)"
fi
[[ -n "$pi_dir" && -f "$pi_dir/package.json" ]] || die "@phux/pi is not installed. Run: pi install npm:@phux/pi@${expected_pi}"
actual_pi="$(pi_package_version "$pi_dir")"
[[ "$actual_pi" == "$expected_pi" ]] || die "@phux/pi ${actual_pi} does not match the @phux/pi ${expected_pi} shipped with phux ${expected_phux}. Install it with: pi install npm:@phux/pi@${expected_pi}"

echo "demo-pi-setup: versions match this checkout: phux ${actual_phux}, @phux/pi ${actual_pi}" >&2

session_exists() {
  "$PHUX_BIN" ls --json | python3 -c 'import json,sys
doc=json.load(sys.stdin)
sessions=doc.get("sessions", [])
raise SystemExit(0 if any(item.get("name")==sys.argv[1] for item in sessions) else 1)' "$SESSION"
}

if session_exists; then
  die "session \"${SESSION}\" already exists. For a clean take: ${PHUX_BIN} kill ${SESSION}"
fi

seed_repo() {
  if [[ -e "$DEMO_DIR" && ! -f "$DEMO_DIR/.phux-pi-demo" ]]; then
    die "$DEMO_DIR exists and is not a phux Pi demo repo"
  fi
  mkdir -p "$DEMO_DIR/bin" "$DEMO_DIR/tests"
  cat >"$DEMO_DIR/bin/add" <<'EOF'
#!/bin/sh
# Deliberately wrong: adds one extra. Shot 1's test fails until this is fixed.
a=${1:?}
b=${2:?}
echo $((a + b + 1))
EOF
  cat >"$DEMO_DIR/tests/add.test.sh" <<'EOF'
#!/bin/sh
# Fails while bin/add adds one extra. The Pi demo's opening frame.
set -eu
root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
got=$("$root/bin/add" 2 2)
if [ "$got" = 4 ]; then
  echo "ok 2+2=4"
  exit 0
fi
echo "FAIL: add 2 2 returned ${got}, expected 4" >&2
exit 1
EOF
  cat >"$DEMO_DIR/README.md" <<'EOF'
# phux Pi demo

`bin/add` is wrong: `add 2 2` prints 5. `tests/add.test.sh` expects 4.
Shot 1 of the demo opens on that failure inside the `work` session.
EOF
  printf '%s\n' "phux pi demo repo" >"$DEMO_DIR/.phux-pi-demo"
  chmod +x "$DEMO_DIR/bin/add" "$DEMO_DIR/tests/add.test.sh"
  if [[ ! -d "$DEMO_DIR/.git" ]]; then
    git -C "$DEMO_DIR" init -q -b main
    git -C "$DEMO_DIR" add -- README.md bin/add tests/add.test.sh .phux-pi-demo
    git -C "$DEMO_DIR" -c user.name=phux-demo -c user.email=demo@example.invalid commit -q -m "Seed a failing add test for the Pi demo"
  fi
  if "$DEMO_DIR/tests/add.test.sh" >/dev/null 2>&1; then
    die "the seeded test passed; shot 1 needs it to fail"
  fi
}

seed_repo

json_field() {
  python3 -c 'import json,sys; print(json.load(sys.stdin)[sys.argv[1]])' "$1"
}

seed_json="$("$PHUX_BIN" new --json -s "$SESSION" -c "$DEMO_DIR")"
seed_id="$(printf '%s' "$seed_json" | json_field terminal_id)"
side_json="$("$PHUX_BIN" spawn --json --target "@${seed_id}" --split vertical --ratio 0.55 -c "$DEMO_DIR")"
side_id="$(printf '%s' "$side_json" | json_field terminal_id)"
"$PHUX_BIN" spawn --json --target "@${side_id}" --split horizontal --ratio 0.5 -c "$DEMO_DIR" >/dev/null

show_failure() {
  local output
  for _ in 1 2 3 4 5; do
    set +e
    output="$("$PHUX_BIN" run --force --json --timeout 30 "@${seed_id}" "sh tests/add.test.sh" 2>/dev/null)"
    set -e
    if printf '%s' "$output" | python3 -c 'import json,sys
doc=json.load(sys.stdin)
raise SystemExit(0 if doc.get("exit_code")==1 and "FAIL:" in doc.get("output","") else 1)'; then
      return 0
    fi
    sleep 0.2
  done
  die "could not show the failing test in @${seed_id}"
}

show_failure

cat <<EOF
Shot 1 is ready. Versions match this checkout: phux ${actual_phux}, @phux/pi ${actual_pi}.

Session "${SESSION}" uses the split from docs/assets/pi-live-fleet.cast
(examples/agents/record-pi-live-fleet):

    seed @${seed_id}  |  side pane          vertical, ratio 0.55
                      |  ----------------    horizontal, ratio 0.5
                      |  lower pane

The seed repo is ${DEMO_DIR}. tests/add.test.sh fails (add 2 2 returns 5,
expected 4) and that failure is on the seed pane.

Start the recorder, then:

    ${PHUX_BIN} attach ${SESSION}

In a terminal that is not one of these panes, start Pi (\`pi\`) and run
/phux. Choose the seed pane. Ask Pi to make tests/add.test.sh pass.
See examples/agents/README.md for the runnable fleet example and its checks.

Reset for another take:

    ${PHUX_BIN} kill ${SESSION}
EOF
