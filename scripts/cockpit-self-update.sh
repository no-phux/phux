#!/bin/sh
#
# POSIX sh on purpose. This is the in-app Check for Updates / Install driver.
# It classifies the running Cockpit bundle, compares it to the head of its
# release channel, and installs by execing scripts/install-cockpit.sh — never
# a second download stack.
#
# Channels: `stable` compares CFBundleShortVersionString to the latest
# cockpit-vX.Y.Z release. `next` compares the bundle's PhuxBuildSHA to the SHA
# the moving `next` prerelease points at. A bundle follows the channel baked
# into its Info.plist (PhuxChannel; absent means stable), so switching is one
# `--channel` install and the choice survives without a separate state file.
#
# The app (and tests) parse the key: value document on stdout. Human-readable
# messages live in `message` / `remedy`. Exit 0 reports, 1 fails, 2 refuses.
set -eu

usage() {
  cat <<'EOF'
Usage: scripts/cockpit-self-update.sh [--check|--install] --bundle <Phux Cockpit.app> [options]

Options:
  --check                Report current vs latest; never install (default).
  --install              Install the latest release through install-cockpit.sh.
  --channel <stable|latest|next>
                         Channel to follow (default: the bundle's own). A
                         different channel than the bundle's is a switch.
  --bundle <app>         Path to the running Phux Cockpit.app.
  --installer <script>   install-cockpit.sh to drive (default: beside this file).
  --current-version <X.Y.Z>
                         Override CFBundleShortVersionString (tests).
  --latest <cockpit-vX.Y.Z|X.Y.Z|next.SHA>
                         Skip GitHub discovery (tests / pinned install).
  --home <dir>           Override $HOME for Applications detection.
  --homebrew-prefix <dir>
                         Override Homebrew prefix (Caskroom detection).
  --nix-store <dir>      Override Nix store prefix.
  --os <darwin>          Forwarded to install-cockpit.sh.
  --arch <arm64|aarch64> Forwarded to install-cockpit.sh.
  --bin-dir <dir>        Forwarded to install-cockpit.sh.
  --help                 Show this help.
EOF
}

die() {
  echo "error: $*" >&2
  exit 1
}

action="check"
channel=""
bundle=""
installer=""
current_override=""
latest_override=""
home_dir="${HOME:-}"
homebrew_prefix="${HOMEBREW_PREFIX:-}"
nix_store="${NIX_STORE:-}"
os=""
arch=""
bin_dir=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --check) action="check"; shift ;;
    --install) action="install"; shift ;;
    --channel)
      [ "$#" -ge 2 ] || die "--channel requires a value"
      channel="$2"
      shift 2
      ;;
    --bundle)
      [ "$#" -ge 2 ] || die "--bundle requires a value"
      bundle="$2"
      shift 2
      ;;
    --installer)
      [ "$#" -ge 2 ] || die "--installer requires a value"
      installer="$2"
      shift 2
      ;;
    --current-version)
      [ "$#" -ge 2 ] || die "--current-version requires a value"
      current_override="$2"
      shift 2
      ;;
    --latest)
      [ "$#" -ge 2 ] || die "--latest requires a value"
      latest_override="$2"
      shift 2
      ;;
    --home)
      [ "$#" -ge 2 ] || die "--home requires a value"
      home_dir="$2"
      shift 2
      ;;
    --homebrew-prefix)
      [ "$#" -ge 2 ] || die "--homebrew-prefix requires a value"
      homebrew_prefix="$2"
      shift 2
      ;;
    --nix-store)
      [ "$#" -ge 2 ] || die "--nix-store requires a value"
      nix_store="$2"
      shift 2
      ;;
    --os)
      [ "$#" -ge 2 ] || die "--os requires a value"
      os="$2"
      shift 2
      ;;
    --arch)
      [ "$#" -ge 2 ] || die "--arch requires a value"
      arch="$2"
      shift 2
      ;;
    --bin-dir)
      [ "$#" -ge 2 ] || die "--bin-dir requires a value"
      bin_dir="$2"
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      die "unknown option: $1"
      ;;
  esac
done

[ -n "$bundle" ] || die "--bundle is required"

self_dir="$(CDPATH='' cd "$(dirname "$0")" && pwd)"
if [ -z "$installer" ]; then
  installer="${self_dir}/install-cockpit.sh"
fi
[ -f "$installer" ] || die "install-cockpit.sh not found at $installer"

resolve_path() {
  target="$1"
  if [ -d "$target" ]; then
    (CDPATH='' cd "$target" && pwd -P)
    return
  fi
  parent="$(dirname "$target")"
  base="$(basename "$target")"
  if [ -d "$parent" ]; then
    printf '%s/%s\n' "$(CDPATH='' cd "$parent" && pwd -P)" "$base"
    return
  fi
  printf '%s\n' "$target"
}

bundle="$(resolve_path "$bundle")"
[ -d "$bundle" ] || die "bundle is not a directory: $bundle"
[ -f "${bundle}/Contents/Info.plist" ] || die "not a Cockpit app bundle: $bundle"
if [ -n "$home_dir" ] && [ -d "$home_dir" ]; then
  home_dir="$(resolve_path "$home_dir")"
fi
if [ -n "$nix_store" ] && [ -d "$nix_store" ]; then
  nix_store="$(resolve_path "$nix_store")"
fi
if [ -n "$homebrew_prefix" ] && [ -d "$homebrew_prefix" ]; then
  homebrew_prefix="$(resolve_path "$homebrew_prefix")"
fi

plist_value() {
  # Prefer plutil on macOS; fall back to a conservative XML scrape for tests.
  if command -v plutil >/dev/null 2>&1; then
    plutil -extract "$2" raw -o - "$1" 2>/dev/null && return
  fi
  awk -v key="<key>$2</key>" '
    index($0, key) { want = 1; next }
    want && /<string>/ {
      gsub(/.*<string>/, "")
      gsub(/<\/string>.*/, "")
      print
      exit
    }
  ' "$1"
}

valid_sha() {
  printf '%s\n' "$1" | LC_ALL=C grep -Eq '^[0-9a-f]{40}$'
}

# `0.29.0` on stable, `0.29.0+next.abc1234` on next (the CLI's spelling).
display() {
  if [ "$2" = "next" ] && [ -n "$3" ]; then
    printf '%s+next.%s\n' "$1" "$(printf '%s' "$3" | cut -c1-7)"
  else
    printf '%s\n' "$1"
  fi
}

valid_semver() {
  printf '%s\n' "$1" | LC_ALL=C grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$'
}

normalize_tag() {
  case "$1" in
    cockpit-v*) printf '%s\n' "$1" ;;
    *) printf 'cockpit-v%s\n' "$1" ;;
  esac
}

semver_from_tag() {
  case "$1" in
    cockpit-v*) printf '%s\n' "${1#cockpit-v}" ;;
    *) printf '%s\n' "$1" ;;
  esac
}

# Compare two X.Y.Z values. stdout: lt, eq, or gt for $1 vs $2.
cmp_semver() {
  left="$1"
  right="$2"
  IFS=.
  # shellcheck disable=SC2086
  set -- $left
  a1=$1 a2=$2 a3=$3
  # shellcheck disable=SC2086
  set -- $right
  b1=$1 b2=$2 b3=$3
  unset IFS
  if [ "$a1" -lt "$b1" ]; then echo lt; return; fi
  if [ "$a1" -gt "$b1" ]; then echo gt; return; fi
  if [ "$a2" -lt "$b2" ]; then echo lt; return; fi
  if [ "$a2" -gt "$b2" ]; then echo gt; return; fi
  if [ "$a3" -lt "$b3" ]; then echo lt; return; fi
  if [ "$a3" -gt "$b3" ]; then echo gt; return; fi
  echo eq
}

path_has_component() {
  needle="$1"
  rest="$2"
  case "$rest" in
    */"$needle"/*|*/"$needle") return 0 ;;
    *) return 1 ;;
  esac
}

macos_exe=""
if [ -x "${bundle}/Contents/MacOS/phux-cockpit-dev" ]; then
  macos_exe="${bundle}/Contents/MacOS/phux-cockpit-dev"
elif [ -x "${bundle}/Contents/MacOS/phux-cockpit" ]; then
  macos_exe="${bundle}/Contents/MacOS/phux-cockpit"
fi
resolved_exe=""
if [ -n "$macos_exe" ]; then
  resolved_exe="$(resolve_path "$macos_exe")"
else
  resolved_exe="$(resolve_path "$bundle")"
fi

if [ -z "$nix_store" ]; then
  nix_store="/nix/store"
fi

if [ -z "$homebrew_prefix" ]; then
  if [ -d /opt/homebrew/Caskroom ]; then
    homebrew_prefix="/opt/homebrew"
  elif [ -d /usr/local/Caskroom ]; then
    homebrew_prefix="/usr/local"
  fi
fi

source="unknown"
remedy=""
applications_dir="$(dirname "$bundle")"
app_name="$(basename "$bundle")"

classify() {
  case "$resolved_exe" in
    "$nix_store"/*)
      source="nix"
      remedy="nix profile upgrade phux-cockpit"
      return
      ;;
  esac
  if path_has_component Caskroom "$resolved_exe"; then
    source="homebrew"
    remedy="brew upgrade --cask no-phux/tap/phux-cockpit"
    return
  fi
  if [ -n "$homebrew_prefix" ] && [ -d "${homebrew_prefix}/Caskroom/phux-cockpit" ]; then
    case "$applications_dir" in
      /Applications|"${home_dir}/Applications")
        source="homebrew"
        remedy="brew upgrade --cask no-phux/tap/phux-cockpit"
        return
        ;;
    esac
  fi
  case "$resolved_exe" in
    *phux-cockpit-dev|*/.dev-run/*|*/zig-out/*)
      source="dev"
      remedy="curl -fsSL https://phux.sh/install-cockpit | sh"
      return
      ;;
  esac
  if [ "$app_name" = "Phux Cockpit.app" ]; then
    case "$applications_dir" in
      /Applications|"${home_dir}/Applications")
        source="direct-release"
        remedy=""
        return
        ;;
    esac
  fi
  source="unknown"
  remedy="curl -fsSL https://phux.sh/install-cockpit | sh"
}

classify

current="$current_override"
if [ -z "$current" ]; then
  current="$(plist_value "${bundle}/Contents/Info.plist" CFBundleShortVersionString || true)"
fi
[ -n "$current" ] || die "could not read CFBundleShortVersionString from $bundle"
valid_semver "$current" || die "CFBundleShortVersionString is not X.Y.Z (got $current)"

installed_channel="$(plist_value "${bundle}/Contents/Info.plist" PhuxChannel || true)"
installed_sha=""
case "$installed_channel" in
  next)
    installed_sha="$(plist_value "${bundle}/Contents/Info.plist" PhuxBuildSHA || true)"
    valid_sha "$installed_sha" || installed_sha=""
    ;;
  *) installed_channel="stable" ;;
esac
if [ -z "$channel" ]; then
  channel="$installed_channel"
fi
case "$channel" in
  latest) channel="stable" ;;
  stable|next) ;;
  *) die "--channel must be stable, latest, or next" ;;
esac
current_label="$(display "$current" "$installed_channel" "$installed_sha")"

emit() {
  status="$1"
  message="$2"
  relaunch="${3:-no}"
  printf 'status: %s\n' "$status"
  printf 'source: %s\n' "$source"
  printf 'channel: %s\n' "$channel"
  printf 'current: %s\n' "$current_label"
  printf 'latest: %s\n' "${latest:-}"
  printf 'applications_dir: %s\n' "$applications_dir"
  printf 'relaunch: %s\n' "$relaunch"
  printf 'message: %s\n' "$message"
  printf 'remedy: %s\n' "$remedy"
}

if [ "$source" != "direct-release" ]; then
  case "$source" in
    homebrew) msg="This copy was installed with Homebrew. Use brew to update it." ;;
    nix) msg="This copy lives in the Nix store, which Cockpit cannot overwrite." ;;
    dev) msg="This is a development build, not an installer-placed release." ;;
    *) msg="This copy was not placed by the Phux Cockpit installer." ;;
  esac
  emit refused "$msg"
  exit 2
fi

run_installer() {
  set -- "$installer" "$@"
  if [ -n "$os" ]; then
    set -- "$@" --os "$os"
  fi
  if [ -n "$arch" ]; then
    set -- "$@" --arch "$arch"
  fi
  if [ -n "$bin_dir" ]; then
    set -- "$@" --bin-dir "$bin_dir"
  fi
  "$@"
}

dry_field() {
  printf '%s\n' "$1" | awk -F': ' -v key="$2" '$1 == key { print $2; exit }'
}

# Resolve the channel head. Stable: `latest` is the cockpit-vX.Y.Z tag.
# Next: `latest` is the display label and `latest_sha` the pointer's SHA.
latest=""
latest_sha=""
if [ "$channel" = "next" ]; then
  latest_version="$current"
  if [ -n "$latest_override" ]; then
    latest_sha="${latest_override#next.}"
  else
    dry_out="$(run_installer --channel next --dry-run)" \
      || die "could not resolve the Cockpit next channel"
    latest_sha="$(dry_field "$dry_out" sha)"
    latest_version="$(dry_field "$dry_out" version)"
  fi
  valid_sha "$latest_sha" || die "the next channel named no SHA (got ${latest_sha})"
  valid_semver "$latest_version" || latest_version="$current"
  latest="$(display "$latest_version" next "$latest_sha")"
else
  if [ -n "$latest_override" ]; then
    latest="$(normalize_tag "$latest_override")"
  else
    dry_out="$(run_installer --channel stable --dry-run)" \
      || die "could not resolve the latest cockpit-vX.Y.Z release"
    latest="$(dry_field "$dry_out" tag)"
  fi
  [ -n "$latest" ] || die "could not resolve the latest cockpit-vX.Y.Z release"
  latest_semver="$(semver_from_tag "$latest")"
  valid_semver "$latest_semver" || die "latest release is not X.Y.Z (got $latest)"
fi

# A channel switch always reinstalls; otherwise stable compares versions and
# next compares SHAs (a next build keeps the last released version).
up_to_date=0
if [ "$channel" = "$installed_channel" ]; then
  if [ "$channel" = "next" ]; then
    [ "$installed_sha" = "$latest_sha" ] && up_to_date=1
  else
    case "$(cmp_semver "$current" "$latest_semver")" in
      eq|gt) up_to_date=1 ;;
    esac
  fi
fi

if [ "$up_to_date" -eq 1 ]; then
  emit current "Phux Cockpit ${current_label} is current (latest ${latest})."
  exit 0
fi

if [ "$action" = "check" ]; then
  if [ "$channel" != "$installed_channel" ]; then
    emit newer "Switching Phux Cockpit to the ${channel} channel installs ${latest} (you have ${current_label})."
  else
    emit newer "Phux Cockpit ${latest} is available (you have ${current_label})."
  fi
  exit 0
fi

if [ "$channel" = "next" ]; then
  set -- --channel next
else
  set -- --channel stable --version "$latest"
fi
if run_installer "$@" --applications-dir "$applications_dir" >&2; then
  emit installed "Installed Phux Cockpit ${latest}. The new app will relaunch; Phux sessions stay on the server." yes
  exit 0
fi
emit failed "install-cockpit.sh could not replace Phux Cockpit. The previous app was left in place."
exit 1
