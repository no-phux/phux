
# --- Linux package repositories ----------------------------------------------

# On Debian, Ubuntu, Fedora and RHEL the stable CLI comes from phux's signed
# apt or dnf repository, so it updates with the system. The signing key is
# repo_key, embedded by install/generate.sh from packaging/phux.asc:
# it is as trusted as this script, and nothing else is fetched to trust it.
# Packages stay on their releases; the signed metadata is the `repodata`
# release (docs/releasing.md#linux-packages).
REPO_URL="${PHUX_REPO_URL:-https://github.com/no-phux/phux/releases/download}"

# apt or dnf when this host is one phux's repositories serve, and the
# installer can act as root: as root, or through sudo that needs no password
# or has a terminal to ask on.
repo_manager() {
  [ "$(uname -s)" = Linux ] && [ -r "${PHUX_OS_RELEASE:-/etc/os-release}" ] || return 1
  # shellcheck disable=SC1090,SC1091
  ids="$(. "${PHUX_OS_RELEASE:-/etc/os-release}" && printf ' %s %s ' "${ID:-}" "${ID_LIKE:-}")"
  case "$ids" in
    *' debian '* | *' ubuntu '*) manager=apt tool=apt-get ;;
    *' fedora '* | *' rhel '* | *' centos '*) manager=dnf tool=dnf ;;
    *) return 1 ;;
  esac
  command -v "$tool" >/dev/null 2>&1 || return 1
  if [ "$(id -u)" != 0 ]; then
    command -v sudo >/dev/null 2>&1 || return 1
    sudo -n true 2>/dev/null || { : </dev/tty; } 2>/dev/null || return 1
  fi
  printf '%s\n' "$manager"
}

# Under curl | sh, stdin is the rest of this script: nothing may read it.
# sudo asks for a password on the terminal, not stdin.
as_root() {
  if [ "$(id -u)" = 0 ]; then "$@" </dev/null; else sudo "$@" </dev/null; fi
}

# Add the repository for MANAGER and install phux from it. Every step is
# chained: a caller's `if` turns off set -e.
install_repo() {
  phux_message info "adding the phux $1 repository, so phux updates with the system (opt out: --no-repo)"
  repo_key > "${work}/phux.asc" || return 1
  case "$1" in
    apt)
      printf 'deb [signed-by=/etc/apt/keyrings/phux.asc] %s repodata/\n' "$REPO_URL" > "${work}/phux.list" \
        && as_root install -d -m 755 /etc/apt/keyrings \
        && as_root install -m 644 "${work}/phux.asc" /etc/apt/keyrings/phux.asc \
        && as_root install -m 644 "${work}/phux.list" /etc/apt/sources.list.d/phux.list \
        && as_root apt-get update -qq -o Dir::Etc::sourcelist=sources.list.d/phux.list \
          -o Dir::Etc::sourceparts=- -o APT::Get::List-Cleanup=0 \
        && as_root env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq phux
      ;;
    dnf)
      printf '[phux]\nname=phux\nbaseurl=%s\nenabled=1\ngpgcheck=1\nrepo_gpgcheck=1\nmetadata_expire=1h\ngpgkey=file:///etc/pki/rpm-gpg/RPM-GPG-KEY-phux\n' \
        "$REPO_URL" > "${work}/phux.repo" \
        && as_root install -D -m 644 "${work}/phux.asc" /etc/pki/rpm-gpg/RPM-GPG-KEY-phux \
        && as_root install -m 644 "${work}/phux.repo" /etc/yum.repos.d/phux.repo \
        && as_root rpm --import /etc/pki/rpm-gpg/RPM-GPG-KEY-phux \
        && as_root dnf install -y -q phux
      ;;
  esac
}
