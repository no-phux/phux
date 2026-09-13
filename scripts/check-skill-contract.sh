#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

mkdir -p "$tmp/home" "$tmp/xdg/phux" "$tmp/cwd"
printf 'this is not valid toml = [\n' > "$tmp/xdg/phux/config.toml"

assert_document() {
  name=$1
  output=$2
  first=$(sed -n '1p' "$output")
  skill_name=$(sed -n '2p' "$output")
  description=$(sed -n '3p' "$output")
  closing=$(awk 'NR > 1 && $0 == "---" { print; exit }' "$output")
  first_heading=$(grep -m1 '^# ' "$output" || true)

  [ "$first" = '---' ]
  [ "$skill_name" = "name: $name" ]
  [ -n "${description#description: }" ]
  grep -Eq '^  version: "[0-9]+\.[0-9]+\.[0-9]+" # x-release-please-version$' "$output"
  [ "$closing" = '---' ]
  [ -n "$first_heading" ]
  [ "$(tail -c 1 "$output" | od -An -tuC | tr -d ' ')" = 10 ]
}

check_binary() {
  name=$1
  bin=$2
  expected=$3
  conflict=$4
  output="$tmp/$name.skill"
  stderr="$tmp/$name.stderr"

  (
    cd "$tmp/cwd"
    HOME="$tmp/home" XDG_CONFIG_HOME="$tmp/xdg" PHUX_SOCKET="$tmp/dead.sock" \
      "$bin" --skill > "$output" 2> "$stderr"
  )
  [ ! -s "$stderr" ]
  cmp "$output" "$expected"
  assert_document "$name" "$output"

  "$bin" -h > "$tmp/$name.help" 2> "$stderr"
  [ ! -s "$stderr" ]
  grep -q -- '--skill' "$tmp/$name.help"

  set +e
  "$bin" $conflict > "$tmp/$name.conflict.out" 2> "$stderr"
  code=$?
  set -e
  [ "$code" -eq 2 ]
  [ ! -s "$tmp/$name.conflict.out" ]
  grep -q -- '--skill' "$stderr"

  set -o pipefail
  "$bin" --skill 2> "$stderr" | head -n 1 > /dev/null
  [ ! -s "$stderr" ]
}

sed '/<!-- phux-skill-region:/d' "$root/.agents/skills/using-phux/SKILL.md" > "$tmp/phux.expected"
cp "$root/.agents/skills/using-phux-mcp/SKILL.md" "$tmp/phux-mcp.expected"

check_binary using-phux "$root/target/debug/phux" "$tmp/phux.expected" '--skill=quick ls'
check_binary using-phux-mcp "$root/target/debug/phux-mcp" "$tmp/phux-mcp.expected" '--skill --schema'

# The ergonomic launcher must be a transparent exec boundary: exact discovery
# bytes, live stdio transport, and no phux config/socket initialization first.
HOME="$tmp/home" XDG_CONFIG_HOME="$tmp/xdg" PHUX_SOCKET="$tmp/dead.sock" \
  "$root/target/debug/phux" mcp --skill > "$tmp/launcher.skill" 2> "$tmp/launcher.stderr"
[ ! -s "$tmp/launcher.stderr" ]
cmp "$tmp/launcher.skill" "$tmp/phux-mcp.expected"

"$root/target/debug/phux" mcp --schema > "$tmp/launcher.schema"
"$root/target/debug/phux-mcp" --schema > "$tmp/direct.schema"
cmp "$tmp/launcher.schema" "$tmp/direct.schema"

printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' \
  | "$root/target/debug/phux" mcp > "$tmp/launcher.rpc" 2> "$tmp/launcher.stderr"
[ ! -s "$tmp/launcher.stderr" ]
grep -q '"tools"' "$tmp/launcher.rpc"
grep -q '"phux_ls"' "$tmp/launcher.rpc"

mkdir -p "$tmp/solo"
cp "$root/target/debug/phux" "$tmp/solo/phux"
set +e
PATH=/usr/bin:/bin "$tmp/solo/phux" mcp --skill > "$tmp/missing.out" 2> "$tmp/missing.err"
code=$?
set -e
[ "$code" -eq 127 ]
[ ! -s "$tmp/missing.out" ]
grep -q 'reinstall phux' "$tmp/missing.err"

echo 'skill contract: phux, phux-mcp, and phux mcp verified'
