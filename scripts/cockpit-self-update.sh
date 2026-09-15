#!/bin/sh
#
# POSIX sh on purpose. This is the in-app Check for Updates / Install driver.
# It classifies the running Cockpit bundle, compares CFBundleShortVersionString
# to the latest cockpit-vX.Y.Z release, and installs by execing
# scripts/install-cockpit.sh — never a second download stack.
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
  --bundle <app>         Path to the running Phux Cockpit.app.
  --installer <script>   install-cockpit.sh to drive (default: beside this file).
  --current-version <X.Y.Z>
                         Override CFBundleShortVersionString (tests).
  --latest <cockpit-vX.Y.Z|X.Y.Z>
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

plist_version() {
  # Prefer plutil on macOS; fall back to a conservative XML scrape for tests.
  if command -v plutil >/dev/null 2>&1; then
    plutil -extract CFBundleShortVersionString raw -o - "$1" 2>/dev/null && return
  fi
  awk '
    /<key>CFBundleShortVersionString<\/key>/ { want = 1; next }
    want && /<string>/ {
      gsub(/.*<string>/, "")
      gsub(/<\/string>.*/, "")
      print
      exit
    }
  ' "$1"
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
  current="$(plist_version "${bundle}/Contents/Info.plist" || true)"
fi
[ -n "$current" ] || die "could not read CFBundleShortVersionString from $bundle"
valid_semver "$current" || die "CFBundleShortVersionString is not X.Y.Z (got $current)"

emit() {
  status="$1"
  message="$2"
  relaunch="${3:-no}"
  printf 'status: %s\n' "$status"
  printf 'source: %s\n' "$source"
  printf 'current: %s\n' "$current"
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

latest=""
if [ -n "$latest_override" ]; then
  latest="$(normalize_tag "$latest_override")"
else
  dry_out="$(run_installer --dry-run)" || die "could not resolve the latest cockpit-vX.Y.Z release"
  latest="$(printf '%s\n' "$dry_out" | awk -F': ' '$1 == "tag" { print $2; exit }')"
fi
[ -n "$latest" ] || die "could not resolve the latest cockpit-vX.Y.Z release"
latest_semver="$(semver_from_tag "$latest")"
valid_semver "$latest_semver" || die "latest release is not X.Y.Z (got $latest)"

order="$(cmp_semver "$current" "$latest_semver")"
case "$order" in
  eq|gt)
    emit current "Phux Cockpit ${current} is current (latest ${latest})."
    exit 0
    ;;
esac

if [ "$action" = "check" ]; then
  emit newer "Phux Cockpit ${latest} is available (you have ${current})."
  exit 0
fi

if run_installer --version "$latest" --applications-dir "$applications_dir" >&2; then
  emit installed "Installed Phux Cockpit ${latest}. The new app will relaunch; Phux sessions stay on the server." yes
  exit 0
fi
emit failed "install-cockpit.sh could not replace Phux Cockpit. The previous app was left in place."
exit 1
