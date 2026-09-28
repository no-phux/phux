# shellcheck shell=bash
#
# Run a locally built Cockpit that macOS and name-based automation can tell
# apart from the installed app. Source it:
#
#   ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
#   . "${ROOT}/scripts/lib/dev-app.sh"
#
# Isolation has four parts:
#   1. Bundle id: `dev_app_stage` rewrites CFBundleIdentifier to `<id>.dev`
#      (the SDK has no option for it) and names it "Phux Cockpit (dev)".
#   2. Process name: the executable becomes `phux-cockpit-dev`, so `pgrep -x`
#      and System Events cannot hit the installed app.
#   3. Config and state: PHUX_COCKPIT_CONFIG / PHUX_COCKPIT_STATE point at
#      dev-owned files (unset still finds your real config via XDG).
#   4. Automation dropbox: it is relative to the app's CWD, so
#      `dev_app_launch` starts the app from the dev home.
# Still shared: the SDK's Application Support state and log paths, keyed by
# the compiled-in bundle id.

# Suffixes applied to the packaged bundle's own values. One set, here, so a
# script that ASSERTS the identity and the script that CREATES it cannot drift.
DEV_APP_ID_SUFFIX=".dev"
DEV_APP_EXECUTABLE_SUFFIX="-dev"
DEV_APP_NAME_SUFFIX=" (dev)"
# The app runner confines raw file effects to app_dirs roots derived from the
# binary's runtime id. Point TMPDIR at the dev home and keep state under this
# temp root so debounced saves are both isolated and admitted.
DEV_APP_RUNTIME_ID="dev.phux.cockpit"

# The bundle every non-dev path on this machine means: what the DMG installs,
# and the thing three days of bug reports were filed against.
# shellcheck disable=SC2034 # public value consumed by sourcing scripts
DEV_APP_INSTALLED_BUNDLE="/Applications/Phux Cockpit.app"

dev_app_die() {
    printf 'dev-app: %s\n' "$*" >&2
    return 1
}

dev_app_state_path() {
    printf '%s/%s/workspace.state\n' "$1" "$DEV_APP_RUNTIME_ID"
}

# Read one Info.plist key. Fails loudly rather than returning an empty string,
# because every caller here is about to compare the result to something.
dev_app_plist_value() {
    local bundle="$1" key="$2"
    /usr/bin/plutil -extract "$key" raw -o - "${bundle}/Contents/Info.plist"
}

# Print `<id> <executable> <name>` for a bundle: the three fields that decide
# whether macOS and `pgrep` consider two bundles the same app. One reader, so
# the check script and the run banner cannot disagree about what identity is.
dev_app_identity() {
    local bundle="$1"
    printf '%s %s %s\n' \
        "$(dev_app_plist_value "$bundle" CFBundleIdentifier)" \
        "$(dev_app_plist_value "$bundle" CFBundleExecutable)" \
        "$(dev_app_plist_value "$bundle" CFBundleName)"
}

# Copy a packaged bundle to `dest_app` (replacing any stale copy) and give it
# its own identity. Prints the staged executable's path.
dev_app_stage() {
    local source_app="$1" dest_app="$2"

    [[ -d "$source_app" ]] || dev_app_die "no packaged bundle at ${source_app}; run zig build package first" || return 1

    local source_executable source_id source_name
    source_executable="$(dev_app_plist_value "$source_app" CFBundleExecutable)" || return 1
    source_id="$(dev_app_plist_value "$source_app" CFBundleIdentifier)" || return 1
    source_name="$(dev_app_plist_value "$source_app" CFBundleName)" || return 1

    # Staging a staged bundle would produce phux-cockpit-dev-dev and an id with
    # two suffixes -- still unique, but no longer the name the docs, the
    # isolation check, and any script you wrote yesterday expect.
    case "$source_executable" in
        *"$DEV_APP_EXECUTABLE_SUFFIX")
            dev_app_die "${source_app} is already a dev bundle (${source_executable})" || return 1
            ;;
    esac

    local dest_executable="${source_executable}${DEV_APP_EXECUTABLE_SUFFIX}"
    rm -rf -- "$dest_app"
    mkdir -p -- "$(dirname -- "$dest_app")"
    /usr/bin/ditto "$source_app" "$dest_app"

    mv -- "${dest_app}/Contents/MacOS/${source_executable}" \
          "${dest_app}/Contents/MacOS/${dest_executable}"

    local plist="${dest_app}/Contents/Info.plist"
    /usr/bin/plutil -replace CFBundleExecutable -string "$dest_executable" "$plist"
    /usr/bin/plutil -replace CFBundleIdentifier -string "${source_id}${DEV_APP_ID_SUFFIX}" "$plist"
    /usr/bin/plutil -replace CFBundleName -string "${source_name}${DEV_APP_NAME_SUFFIX}" "$plist"
    /usr/bin/plutil -replace CFBundleDisplayName -string "${source_name}${DEV_APP_NAME_SUFFIX}" "$plist"
    /usr/bin/plutil -lint "$plist" >/dev/null

    # Re-sign: renaming the binary and rewriting the plist breaks the adhoc
    # signature, and macOS then never delivers a keystroke to the process.
    /usr/bin/codesign --force --deep --timestamp=none --sign - "$dest_app" 2>/dev/null \
        || dev_app_die "could not adhoc re-sign ${dest_app}; without a valid signature macOS will not give it key focus and it will accept no keyboard input" \
        || return 1

    # The staged bundle must verify cleanly; keyboard input depends on it.
    local dest_verify=0
    /usr/bin/codesign --verify --deep --strict "$dest_app" 2>/dev/null || dest_verify=$?
    if [[ "$dest_verify" != 0 ]]; then
        dev_app_die "staged bundle ${dest_app} fails codesign --verify (${dest_verify}); macOS will not give it key focus and it will accept no keyboard input" || return 1
    fi

    printf '%s\n' "${dest_app}/Contents/MacOS/${dest_executable}"
}

# Create the dev home: its own config file (never seeded from yours, since
# Cockpit writes theme choices back) and the state/dropbox directory.
dev_app_home_init() {
    local home="$1"
    mkdir -p -- "$home" "${home}/${DEV_APP_RUNTIME_ID}"
    if [[ ! -e "${home}/config" ]]; then
        cat > "${home}/config" <<'CONFIG'
# Config for LOCAL DEV RUNS only (scripts/dev-run.sh). Your real config at
# ~/.config/phux-cockpit/config is never read or written by a dev run, and this
# file is never read by the installed app. Same keys; see README "Configuration".
#
# font-size = 13
# theme = tokyonight
CONFIG
    fi
}

# Launch a staged dev build from inside the dev home (`env -C`, so only the
# child's CWD moves). Trailing args are extra KEY=value env entries. Sets
# DEV_APP_PID.
dev_app_launch() {
    local executable="$1" home="$2" config="$3" log="$4"
    shift 4
    env -C "$home" \
        TMPDIR="$home" \
        PHUX_COCKPIT_CONFIG="$config" \
        PHUX_COCKPIT_STATE="$(dev_app_state_path "$home")" \
        "$@" \
        "$executable" >"$log" 2>&1 &
    # shellcheck disable=SC2034 # output variable consumed by the caller
    DEV_APP_PID=$!
}

# Wait until `pgrep -x <name>` reports exactly the pid we launched (`-x`
# because `-f` self-matches the shell running it).
dev_app_wait_named() {
    local name="$1" want_pid="$2" deadline=$((SECONDS + 20))
    while :; do
        local found
        found="$(pgrep -x "$name" | tr '\n' ' ' | sed 's/ $//')"
        [[ "$found" == "$want_pid" ]] && return 0
        if [[ "$SECONDS" -ge "$deadline" ]]; then
            printf "dev-app: waited 20s for \`pgrep -x %s\` to be exactly %s, got: %s\n" \
                "$name" "$want_pid" "${found:-<nothing>}" >&2
            return 1
        fi
        sleep 0.2
    done
}
