#!/usr/bin/env bash
# Instruction-count regression guard.
#
# Builds the phux-server `icount` bench with the `icount` profile (release
# optimizations, no fat LTO) and runs it under callgrind. Compares Ir to
# benchmarks/icount-baseline.json. `--update` rewrites that file from the
# run; committing the rewrite on a pull request needs the trailer or label
# described in docs/performance-ci.md.
#
# Valgrind comes from the flake's nixpkgs so its libc matches the devshell
# that built the bench. Counts from another libc or architecture are not
# comparable.
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

UPDATE=0
if [[ $# -gt 1 ]]; then
  echo "usage: scripts/ci/perf-icount.sh [--update]" >&2
  exit 2
fi
if [[ $# -eq 1 ]]; then
  if [[ $1 != "--update" ]]; then
    echo "usage: scripts/ci/perf-icount.sh [--update]" >&2
    exit 2
  fi
  UPDATE=1
fi

if ! command -v nix >/dev/null 2>&1; then
  echo "perf-icount: nix is required so valgrind matches the devshell libc" >&2
  exit 1
fi
valgrind_bin="$(nix shell --inputs-from "$ROOT" nixpkgs#valgrind -c bash -c 'command -v valgrind')"
valgrind_dir="$(dirname "$valgrind_bin")"
PATH="${valgrind_dir}:${PATH}"
export PATH
command -v valgrind >/dev/null 2>&1

# The library spawns this binary. Its version has to match the
# iai-callgrind crate in Cargo.lock. It is not a workspace member: the
# bench only needs it on PATH.
runner_version="$(bun tools/main.ts perf-icount runner-version)"
runner_root="${ROOT}/target/icount-runner"
if [[ ! -x ${runner_root}/bin/iai-callgrind-runner ]]; then
  cargo install --locked --version "${runner_version}" --profile release \
    --target-dir "${ROOT}/target/icount-runner-build" \
    --root "${runner_root}" \
    iai-callgrind-runner
fi
PATH="${runner_root}/bin:${PATH}"
export PATH

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

if [[ $UPDATE -eq 1 ]]; then
  bun tools/main.ts perf-icount update \
    --measured "${out}/iai.jsonl" \
    --baseline "${ROOT}/benchmarks/icount-baseline.json"
else
  bun tools/main.ts perf-icount compare \
    --measured "${out}/iai.jsonl" \
    --baseline "${ROOT}/benchmarks/icount-baseline.json"
fi

bun tools/main.ts perf-icount authorize \
  --base "${PHUX_BASE_SHA:-}" \
  --event "${GITHUB_EVENT_NAME:-}" \
  --label-update "${PHUX_PERF_BASELINE_UPDATE:-0}"
