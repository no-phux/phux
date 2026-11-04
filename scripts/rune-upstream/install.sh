#!/usr/bin/env bash
# Install the Rune upstream follower on this Mac (scripts/rune-upstream/README.md):
# its home directory, an upstream mirror, and a LaunchAgent that runs tick.sh.
# Rerun it after tick.sh changes on main; it never touches the home's
# worktree, journal or steering file once they exist.
#
# Usage: scripts/rune-upstream/install.sh [--hours "7 19"] [--home DIR] [--profile NAME] [--uninstall]
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(git -C "$here" worktree list --porcelain | sed -n '1s/^worktree //p')"
home="${RUNE_UPSTREAM_HOME:-$(dirname "$repo")/rune-upstream}"
hours="7 19"
profile=""
label=sh.phux.rune-upstream
plist="$HOME/Library/LaunchAgents/$label.plist"
domain="gui/$(id -u)"
uninstall=0

die() { echo "install.sh: $*" >&2; exit 2; }
xml() { sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' <<<"$1"; }

while (($#)); do
    case "$1" in
        --hours) hours="${2:?--hours needs a list like \"7 19\"}"; shift 2 ;;
        --home) home="${2:?--home needs a directory}"; shift 2 ;;
        --profile) profile="${2:?--profile needs a phux profile name}"; shift 2 ;;
        --uninstall) uninstall=1; shift ;;
        -h | --help) sed -n '2,7p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) die "unknown argument: $1" ;;
    esac
done

[[ "$(uname -s)" == Darwin ]] || die "launchd only; this is $(uname -s)"
if ((uninstall)); then
    launchctl bootout "$domain/$label" 2>/dev/null || true
    rm -f "$plist"
    echo "removed $label; $home and the phux session are left as they are"
    exit 0
fi

calendar=""
for hour in $hours; do
    [[ "$hour" =~ ^([01]?[0-9]|2[0-3])$ ]] || die "bad hour: $hour"
    calendar+="<dict><key>Hour</key><integer>$((10#$hour))</integer><key>Minute</key><integer>0</integer></dict>"
done
[[ -n "$calendar" ]] || die "--hours is empty"

mkdir -p "$home/bin" "$(dirname "$plist")"
home="$(cd "$home" && pwd)"
install -m 0755 "$here/tick.sh" "$home/bin/tick.sh"

# Config above the worktree applies to OpenCode runs in it and nowhere else.
[[ -f "$home/opencode.jsonc" ]] || cat >"$home/opencode.jsonc" <<'EOF'
{
  "$schema": "https://opencode.ai/config.json",
  "default_agent": "rune-upstream"
}
EOF

[[ -f "$home/STEERING.md" ]] || cat >"$home/STEERING.md" <<'EOF'
# Steering

Standing orders for the rune-upstream agent. Edit this file, or tell a live
run and it writes the order here. The agent reads it first on every run;
newer lines win.

- release: off  (`auto` lets a run cut the next Phux Rune alpha after it
  merges a change users would notice; docs/releasing.md, Phux Rune)
EOF

[[ -d "$home/rune.git" ]] || git clone -q --mirror --filter=blob:none https://github.com/anomalyco/rune "$home/rune.git"

profile_env=""
[[ -z "$profile" ]] || profile_env="<key>PHUX_PROFILE</key><string>$(xml "$profile")</string>"
path="$HOME/.local/bin:$HOME/.opencode/bin:${MISE_DATA_DIR:-$HOME/.local/share/mise}/shims:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
staged="$(mktemp)"
cat >"$staged" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$label</string>
  <key>ProgramArguments</key><array><string>/bin/bash</string><string>$(xml "$home/bin/tick.sh")</string></array>
  <key>StartCalendarInterval</key><array>$calendar</array>
  <key>EnvironmentVariables</key><dict>
    <key>PATH</key><string>$(xml "$path")</string>
    <key>RUNE_UPSTREAM_HOME</key><string>$(xml "$home")</string>
    <key>RUNE_UPSTREAM_REPO</key><string>$(xml "$repo")</string>
    $profile_env
  </dict>
  <key>StandardOutPath</key><string>$(xml "$home/tick.log")</string>
  <key>StandardErrorPath</key><string>$(xml "$home/tick.log")</string>
</dict>
</plist>
EOF
plutil -lint -s "$staged"
mv "$staged" "$plist"

# A job that was just booted out can refuse a bootstrap while it winds down.
launchctl bootout "$domain/$label" 2>/dev/null || true
for attempt in 1 2 3 4 5; do
    launchctl bootstrap "$domain" "$plist" 2>/dev/null && break
    ((attempt < 5)) || die "launchctl bootstrap $domain $plist failed"
    sleep 1
done

echo "installed $label: ticks at hours $hours, home $home${profile:+, phux profile $profile}"
echo "run one now: launchctl kickstart $domain/$label"
