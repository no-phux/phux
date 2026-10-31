
# --- main --------------------------------------------------------------------

channel="${PHUX_CHANNEL:-stable}"
pin=""
pinned_flag=""
pin_sha256=""
install_dir="${PHUX_INSTALL_DIR:-${HOME:-}/.local/bin}"
applications_dir="${PHUX_DESKTOP_APPLICATIONS_DIR:-}"
stage_dir=""
desktop=0
rune=0
dry_run=0
os=""
arch=""
modify_path=1
case "${PHUX_NO_MODIFY_PATH:-}" in '' | 0) ;; *) modify_path=0 ;; esac

while [ "$#" -gt 0 ]; do
  case "$1" in
    --channel | --version | --revision | --sha256 | --install-dir | --applications-dir | --stage-dir | --os | --arch)
      [ "$#" -ge 2 ] && [ -n "$2" ] || die "$1 requires a value"
      ;;
  esac
  case "$1" in
    --channel) channel="$2" ;;
    --version)
      pin="${2#desktop-}"
      pin="${pin#v}"
      pinned_flag=--version
      ;;
    --revision) pin="$2" pinned_flag=--revision ;;
    --sha256) pin_sha256="$2" ;;
    --install-dir) install_dir="$2" ;;
    --applications-dir) applications_dir="$2" ;;
    --stage-dir) stage_dir="$2" ;;
    --os) os="$2" ;;
    --arch) arch="$2" ;;
    --desktop) desktop=1 ;;
    --rune) rune=1 ;;
    --dry-run) dry_run=1 ;;
    --no-modify-path) modify_path=0 ;;
    --help | -h) usage; exit 0 ;;
    *) die "unknown option: $1" ;;
  esac
  case "$1" in --desktop | --rune | --dry-run | --no-modify-path) shift ;; *) shift 2 ;; esac
done

if [ "$rune" -eq 1 ]; then
  [ "$desktop" -eq 0 ] && [ "$channel" != next ] && [ -z "$pin" ] \
    || die "--desktop, --channel next and pins do not combine with --rune"
fi
case "$channel" in
  latest) channel=stable ;;
  stable | next) ;;
  *) die "--channel must be stable, latest, or next" ;;
esac
if [ -n "$pin" ]; then
  if [ "$channel" = next ]; then
    [ "$pinned_flag" = --revision ] && matches "$pin" '[0-9a-f]{40}' \
      || die "on the next channel, pin a build with --revision <40-character commit>"
  else
    [ "$pinned_flag" = --version ] && matches "$pin" "$SEMVER" \
      || die "on the stable channel, pin a release with --version X.Y.Z"
  fi
fi
[ -z "$pin_sha256" ] || { [ -n "$pin" ] && matches "$pin_sha256" '[0-9a-f]{64}'; } \
  || die "--sha256 needs 64 lowercase hex digits and a --version or --revision pin"
[ -n "$install_dir" ] || die "--install-dir resolved to an empty path"
if [ -n "$stage_dir" ]; then
  [ "$desktop" -eq 1 ] || die "--stage-dir stages Phux Desktop; pass --desktop"
  case "$stage_dir" in /?*) stage_dir="${stage_dir%/}" ;; *) die "--stage-dir must be an absolute directory other than /" ;; esac
  # Stage on the volume the app swaps into, under the same lock.
  applications_dir="$(dirname "$stage_dir")"
elif [ -z "$applications_dir" ]; then
  case "${os:-$(uname -s)}" in
    [Ll]inux) applications_dir="$(linux_data_home)" ;;
    *) if [ -w /Applications ]; then applications_dir=/Applications; else applications_dir="${HOME:-}/Applications"; fi ;;
  esac
fi

case "${os:-$(uname -s)}/${arch:-$(uname -m)}" in
  [Dd]arwin/arm64 | [Dd]arwin/aarch64) target=aarch64-apple-darwin ;;
  [Ll]inux/x86_64 | [Ll]inux/amd64) target=x86_64-unknown-linux-gnu ;;
  [Ll]inux/arm64 | [Ll]inux/aarch64) target=aarch64-unknown-linux-gnu ;;
  *) die "no phux release for ${os:-$(uname -s)}/${arch:-$(uname -m)}; build from source" ;;
esac

work="$(mktemp -d)"
cli_locked=0
cli_publishing=0
cli_published=""
cli_publish=""
app_publishing=0
app_published=0
app_publish=""
app_locked=0
app_lock=""
stage_tmp=""
cleanup() {
  rollback_cli
  rollback_desktop || true
  rm -rf "$work" "$cli_publish" "$app_publish" "$stage_tmp"
  [ "$cli_locked" -eq 0 ] || rmdir "${install_dir}/.phux-install.lock" 2>/dev/null || true
  [ "$app_locked" -eq 0 ] || rmdir "$app_lock" 2>/dev/null || true
}
trap cleanup EXIT
trap_signals

if [ "$rune" -eq 1 ]; then
  install_rune
elif [ "$desktop" -eq 1 ]; then
  install_desktop
else
  install_cli "$pin" "$pin_sha256"
  [ "$dry_run" -eq 1 ] || report_path
fi
