#!/usr/bin/env bash
# Instruction-count regression guard; see docs/development.md#performance.
#
# Builds the phux-server `icount` bench with the `icount` profile and runs it
# under callgrind, then compares Ir to benchmarks/icount-baseline.json, or
# rewrites that file with `--update`. Valgrind comes from the flake's nixpkgs
# so its libc matches the devshell that built the bench.
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

case "$*" in
  "") action=compare ;;
  --update) action=update ;;
  *)
    echo "usage: scripts/ci/perf-icount.sh [--update]" >&2
    exit 2
    ;;
esac

# Pure git policy: refuse an unauthorized baseline edit before the slow bench.
bun tools/main.ts perf-icount authorize

if ! command -v nix >/dev/null 2>&1; then
  echo "perf-icount: nix is required so valgrind matches the devshell libc" >&2
  exit 1
fi
valgrind_bin="$(nix shell --inputs-from "$ROOT" nixpkgs#valgrind -c bash -c 'command -v valgrind')"
PATH="$(dirname "$valgrind_bin"):${PATH}"

# The bench spawns iai-callgrind-runner, whose version must match the
# iai-callgrind crate in Cargo.lock.
runner_version="$(bun tools/main.ts perf-icount runner-version)"
runner_root="${ROOT}/target/icount-runner"
if [[ ! -x ${runner_root}/bin/iai-callgrind-runner ]]; then
  cargo install --locked --version "${runner_version}" --profile release \
    --target-dir "${ROOT}/target/icount-runner-build" --root "${runner_root}" iai-callgrind-runner
fi
export PATH="${runner_root}/bin:${PATH}"

out="${ROOT}/target/perf-icount"
mkdir -p "$out"
: > "${out}/iai.jsonl"

cargo build --locked --profile icount -p phux-server --bench icount
# Newest matching file: incremental builds leave earlier hashes beside it.
executable="$(
  find "${ROOT}/target/icount/deps" -type f -name 'icount-*' -executable ! -name '*.d' -printf '%T@\t%p\n' \
    | sort -n | tail -1 | cut -f2-
)"
if [[ -z ${executable} || ! -x ${executable} ]]; then
  echo "perf-icount: icount bench executable not found under target/icount/deps" >&2
  exit 1
fi

"$executable" --output-format=json --home="${out}/iai" > "${out}/iai.jsonl"
bun tools/main.ts perf-icount "$action" "${out}/iai.jsonl"
