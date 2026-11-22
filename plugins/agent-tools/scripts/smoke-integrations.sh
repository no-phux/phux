#!/bin/sh
set -eu

script_dir=$(CDPATH=; cd -- "$(dirname -- "$0")" && pwd)
tmp=${TMPDIR:-/tmp}/phux-agent-tools-smoke.$$
state_dir=$tmp/state
fake_bin=$tmp/bin

cleanup() {
  rm -rf "$tmp"
}
trap cleanup EXIT HUP INT TERM

mkdir -p "$state_dir" "$fake_bin"
printf '#!/bin/sh\nprintf fake-codex\n' > "$fake_bin/codex"
printf '#!/bin/sh\nprintf fake-claude\n' > "$fake_bin/claude"
chmod +x "$fake_bin/codex" "$fake_bin/claude"

PHUX_AGENT_TOOLS_STATE_DIR=$state_dir sh "$script_dir/validate-integrations.sh" >/dev/null
PHUX_AGENT_TOOLS_STATE_DIR=$state_dir sh "$script_dir/list-integrations.sh" >/dev/null

PHUX_AGENT_TOOLS_STATE_DIR=$state_dir \
PHUX_AGENT_PACKAGES="codex claude-code" \
  sh "$script_dir/link-integration.sh" > "$tmp/link.tsv"

PHUX_AGENT_TOOLS_STATE_DIR=$state_dir \
PHUX_AGENT_TOOLS_DETECT=1 \
PHUX_AGENT_TOOLS_PATH=$fake_bin \
  sh "$script_dir/status-integrations.sh" > "$tmp/status-current.tsv"

awk -F '\t' 'NR > 1 && ($1 == "codex" || $1 == "claude-code") && $5 != "current" { exit 1 }' "$tmp/status-current.tsv"
awk -F '\t' 'NR > 1 && ($1 == "codex" || $1 == "claude-code") && $7 != "available" { exit 1 }' "$tmp/status-current.tsv"

awk -F '=' '
  $1 == "package_version" { print "package_version=0.0.0"; next }
  { print }
' "$state_dir/codex.link" > "$tmp/codex.link"
mv -f "$tmp/codex.link" "$state_dir/codex.link"

PHUX_AGENT_TOOLS_STATE_DIR=$state_dir \
  sh "$script_dir/status-integrations.sh" > "$tmp/status-outdated.tsv"

awk -F '\t' 'NR > 1 && $1 == "codex" && $5 != "outdated" { exit 1 }' "$tmp/status-outdated.tsv"

PHUX_AGENT_TOOLS_STATE_DIR=$state_dir \
PHUX_AGENT_PACKAGES="codex claude-code" \
  sh "$script_dir/unlink-integration.sh" > "$tmp/unlink.tsv"

PHUX_AGENT_TOOLS_STATE_DIR=$state_dir \
  sh "$script_dir/status-integrations.sh" > "$tmp/status-missing.tsv"

awk -F '\t' 'NR > 1 && ($1 == "codex" || $1 == "claude-code") && $5 != "missing" { exit 1 }' "$tmp/status-missing.tsv"

printf 'agent integration smoke ok\n'
