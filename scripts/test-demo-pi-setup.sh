#!/usr/bin/env bash
# Exercise scripts/demo-pi-setup.sh against a fake phux and a fake @phux/pi.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
scratch="$(mktemp -d "${TMPDIR:-/tmp}/phux-demo-pi-test.XXXXXX")"
scratch="$(cd "$scratch" && pwd)"
trap 'rm -rf "$scratch"' EXIT

expected_phux="$(
  python3 - "$root/Cargo.toml" <<'PY'
import pathlib, sys
for line in pathlib.Path(sys.argv[1]).read_text().splitlines():
    if line.startswith("version = "):
        print(line.split('"')[1])
        break
PY
)"
expected_pi="$(
  python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' "$root/integrations/pi/package.json"
)"

bin="$scratch/bin"
mkdir -p "$bin"
cat >"$bin/phux" <<EOF
#!/usr/bin/env bash
set -euo pipefail
log="\${PHUX_ARGV_LOG:?}"
if [[ "\${1:-}" == "--version" ]]; then
  printf 'phux %s\\n' "\${PHUX_FAKE_VERSION:-${expected_phux}}"
  exit 0
fi
printf '%s\\n' "\$*" >>"\$log"
if [[ "\${1:-}" == "ls" ]]; then
  if [[ "\${PHUX_FAKE_HAS_WORK:-}" == 1 ]]; then
    printf '%s\\n' '{"schema_version":1,"sessions":[{"name":"work"}]}'
  else
    printf '%s\\n' '{"schema_version":1,"sessions":[]}'
  fi
  exit 0
fi
if [[ "\${1:-}" == "new" ]]; then
  printf '%s\\n' '{"schema_version":1,"session":"work","terminal_id":1}'
  exit 0
fi
if [[ "\${1:-}" == "spawn" ]]; then
  n=2
  [[ -f "\${PHUX_SPAWN_COUNT:-}" ]] && n="\$(( \$(cat "\$PHUX_SPAWN_COUNT") + 1 ))"
  printf '%s\\n' "\$n" >"\${PHUX_SPAWN_COUNT:?}"
  printf '{"schema_version":1,"terminal_id":%s,"satellite":null,"replayed":false}\\n' "\$n"
  exit 0
fi
if [[ "\${1:-}" == "run" ]]; then
  printf '%s\\n' '{"command":"sh tests/add.test.sh","exit_code":1,"output":"FAIL: add 2 2 returned 5, expected 4","duration_ms":1,"truncated":false}'
  exit 1
fi
echo "unexpected phux args: \$*" >&2
exit 2
EOF
chmod +x "$bin/phux"

home="$scratch/home"
mkdir -p "$home/.pi/agent/npm/node_modules/@phux/pi"
printf '{"name":"@phux/pi","version":"%s"}\n' "$expected_pi" >"$home/.pi/agent/npm/node_modules/@phux/pi/package.json"

log="$scratch/argv"
: >"$log"
demo="$scratch/demo"
out="$scratch/out"
err="$scratch/err"

run_setup() {
  env ${PHUX_PI_PACKAGE+PHUX_PI_PACKAGE="$PHUX_PI_PACKAGE"} \
    HOME="$home" \
    PATH="$bin:/usr/bin:/bin" \
    PHUX="$bin/phux" \
    PHUX_ARGV_LOG="$log" \
    PHUX_SPAWN_COUNT="$scratch/spawns" \
    PHUX_PI_DEMO_DIR="$demo" \
    PHUX_FAKE_VERSION="${PHUX_FAKE_VERSION:-$expected_phux}" \
    PHUX_FAKE_HAS_WORK="${PHUX_FAKE_HAS_WORK:-}" \
    bash "$root/scripts/demo-pi-setup.sh" >"$out" 2>"$err"
}

run_setup
grep -q "Shot 1 is ready" "$out"
grep -q "vertical, ratio 0.55" "$out"
grep -q "horizontal, ratio 0.5" "$out"
grep -q "attach work" "$out"
grep -Fq "$demo" "$out"
grep -q "versions match this checkout: phux ${expected_phux}, @phux/pi ${expected_pi}" "$err"
grep -q "new --json -s work -c ${demo}" "$log"
grep -q "spawn --json --target @1 --split vertical --ratio 0.55 -c ${demo}" "$log"
grep -q "spawn --json --target @2 --split horizontal --ratio 0.5 -c ${demo}" "$log"
grep -q "run --force --json --timeout 30 @1 sh tests/add.test.sh" "$log"
test -x "$demo/tests/add.test.sh"
if "$demo/tests/add.test.sh" >/dev/null 2>"$scratch/test-err"; then
  echo "seeded test should fail" >&2
  exit 1
fi
grep -q "FAIL:" "$scratch/test-err"
test -d "$demo/.git"

# A second take refuses the existing session and does not spawn again.
: >"$log"
PHUX_FAKE_HAS_WORK=1
if run_setup; then
  echo "existing session should fail the setup" >&2
  exit 1
fi
grep -q "already exists" "$err"
if grep -q "spawn --json" "$log"; then
  echo "setup mutated the layout after refusing the session" >&2
  exit 1
fi

# A mismatched phux binary stops before creating a session.
: >"$log"
PHUX_FAKE_HAS_WORK=
PHUX_FAKE_VERSION=0.0.1
rm -rf "$demo"
if run_setup; then
  echo "mismatched phux version should fail" >&2
  exit 1
fi
grep -q "does not match this checkout's phux ${expected_phux}" "$err"
if [[ -s "$log" ]]; then
  echo "version mismatch must not talk to the server" >&2
  cat "$log" >&2
  exit 1
fi

# A mismatched @phux/pi stops the same way. pi is absent, so no reinstall.
PHUX_FAKE_VERSION="$expected_phux"
printf '{"name":"@phux/pi","version":"9.9.9"}\n' >"$home/.pi/agent/npm/node_modules/@phux/pi/package.json"
if run_setup; then
  echo "mismatched @phux/pi should fail" >&2
  exit 1
fi
grep -q "@phux/pi 9.9.9 does not match" "$err"
if [[ -s "$log" ]]; then
  echo "@phux/pi mismatch must not talk to the server" >&2
  exit 1
fi

# An explicit package path that is not @phux/pi, and no pi binary to install it.
PHUX_PI_PACKAGE="$scratch/not-a-package"
mkdir -p "$PHUX_PI_PACKAGE"
if run_setup; then
  echo "missing @phux/pi should fail" >&2
  exit 1
fi
grep -q "pi install npm:@phux/pi@${expected_pi}" "$err"

echo "demo-pi-setup tests passed"
