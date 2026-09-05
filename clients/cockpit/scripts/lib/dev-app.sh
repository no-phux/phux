# shellcheck shell=bash
#
# Shared machinery for running a LOCALLY BUILT Cockpit that macOS, your eyes,
# and every name-based automation script can tell apart from the copy in
# /Applications. Source it, do not run it:
#
#   ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
#   . "${ROOT}/scripts/lib/dev-app.sh"
#
# "Does not clash" is not one property. It is four, each with its own mechanism,
# and getting three of them right still leaves a build you can mistake for the
# installed app.
#
# 1. BUNDLE IDENTITY. `zig build package` stamps app.zon's `.id` into
#    CFBundleIdentifier, so a local build carries the SAME identity as the
#    shipped app -- dev.phux.cockpit, verbatim, verified by
#    `plutil -p zig-out/package/phux-cockpit.app/Contents/Info.plist`.
#    LaunchServices, the Dock, the app switcher and `open -b` all key off that
#    string, so to macOS the two are one app wearing two paths. The SDK offers
#    no build option for it (`grep 'b.option' build.zig` in the pinned SDK lists
#    platform, trace, automation, web-engine, cef, signing -- no bundle id), so
#    `dev_app_stage` rewrites the packaged bundle's plist afterwards: `<id>.dev`,
#    and a bundle name the Dock tile and application menu show as
#    "Phux Cockpit (dev)".
#
# 2. PROCESS NAME. This is the one that bites tooling rather than eyes, and it
#    is invisible until it has already lied to you. `pgrep -x phux-cockpit` and
#    `osascript -e 'tell application "System Events" to ... process
#    "phux-cockpit"'` both target BY NAME, and that name is the executable
#    file's name inside Contents/MacOS. Two live instances sharing it means
#    activation, screenshots and key delivery land on an arbitrary one and
#    nothing reports the substitution -- see phux-cockpit-2ml.10.
#    `dev_app_stage` renames the executable to `phux-cockpit-dev` and points
#    CFBundleExecutable at it, which moves both `pgrep -x` and System Events.
#
# 3. CONFIG AND STATE. `PHUX_COCKPIT_CONFIG` and `PHUX_COCKPIT_STATE` each name
#    a FILE and win outright over every search path, for writes as well as reads
#    (`cockpit/startup.zig` config and state resolution). Setting both is what
#    keeps a dev build from restoring -- and then overwriting -- the workspace
#    the installed app was about to open, and from writing a theme change back
#    into the config file you actually use. Note that leaving them unset does
#    NOT fall back to something harmless: the dotfile lookup reads
#    $XDG_CONFIG_HOME first, which is set on this machine, so an "isolated by
#    HOME" run still finds your real config.
#
# 4. THE AUTOMATION DROPBOX. It has no environment seam at all: the app opens
#    `.zig-cache/native-sdk-automation` relative to its CURRENT WORKING
#    DIRECTORY (pinned SDK, src/app_runner/root.zig, `automation.Server.init`),
#    and the CLI resolves the same relative path from wherever you run it. The
#    only way to give a run its own dropbox is to launch it from its own
#    directory, which is why `dev_app_launch` chdirs rather than exporting.
#
# WHAT THIS IS NOT. Two paths stay shared with the installed app, because the
# SDK keys them off the bundle id COMPILED IN from app.zon rather than the one
# in the plist: `~/Library/Application Support/dev.phux.cockpit/State/windows.zon`
# and `~/Library/Logs/dev.phux.cockpit/native-sdk.jsonl` (observed by running a
# staged bundle under a scratch HOME and listing what appeared). Nothing reads
# windows.zon back -- app.zon sets `restore_state = false` on the main window --
# so the cost today is a shared log file and a window frame nobody restores.
# Isolating them would take an app.zon change or an SDK option that does not
# exist.
#
# RELATED: scripts/lib/measure.sh composes `dev_app_stage` and `dev_app_launch`
# for driven measurements, so those runs inherit all four guarantees rather
# than maintaining a second, weaker launcher.

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

# Copy a packaged bundle to `dest_app` and give the copy its own identity.
# Prints the path of the staged executable, which is what you launch.
#
# Idempotent by demolition: the destination is removed first, so a stale bundle
# from an older build can never be the thing that starts.
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

    # That rewriting the plist and renaming the executable costs no signature
    # validity, checked every time instead of trusted -- and checked
    # DIFFERENTIALLY, because today the answer for both bundles is "invalid":
    #
    #   codesign --verify --deep --strict zig-out/package/phux-cockpit.app
    #     -> exit 1, "code has no resources but signature indicates they must
    #        be present"                                    (measured 2026-08-12)
    #
    # `zig build package` emits an adhoc LINKER-signed binary in a bundle with
    # no _CodeSignature at all (`codesign -dv` says `Info.plist=not bound`,
    # `Sealed Resources=none`); scripts/package-macos.sh is what signs a release
    # properly, afterwards. An absolute --verify here would assert something
    # that was never true and so could never fail for our reason. What must hold
    # is that staging does not make it WORSE -- and the day the SDK starts
    # sealing resources, source becomes valid, staged does not, and this fires
    # with the fix named.
    # RE-SIGN. This is not hygiene, it is the difference between an app you can
    # type into and one you cannot.
    #
    # `zig build package` emits an adhoc LINKER-signed binary. Renaming that
    # binary and rewriting the Info.plist around it breaks the signature that
    # was computed over both, and macOS then declines to make the process a
    # proper foreground app: the window appears and paints, `focused=true` shows
    # on the window, the shell runs -- and NOT ONE KEYSTROKE arrives. Cockpit's
    # own `handleKey` opens with `if (!model.focused) return;`, which drops every
    # key with no counter, no log and no error, so the app looks healthy while
    # being completely deaf.
    #
    # Measured 2026-08-14: identical build, plain bundle takes input (typed `ec`
    # reached the shell and autosuggested from history); staged bundle took
    # nothing -- five keystrokes, five chords, dispatch_errors=0, no error event.
    # Re-signing is what closed the gap. See phux-cockpit-2ml.10 for why the
    # silence made this expensive to find.
    /usr/bin/codesign --force --deep --timestamp=none --sign - "$dest_app" 2>/dev/null \
        || dev_app_die "could not adhoc re-sign ${dest_app}; without a valid signature macOS will not give it key focus and it will accept no keyboard input" \
        || return 1

    # Now that staging re-signs, the invariant is ABSOLUTE rather than
    # differential: the staged bundle must verify cleanly, full stop. That is
    # the property keyboard input actually depends on.
    #
    # It is deliberately NOT compared against the source any more. The source is
    # `zig build package` output, which does not seal its resources and verifies
    # 1; the staged bundle verifies 0. A differential check reads that
    # improvement as a change and fails -- which it did, on the first run after
    # the re-sign landed.
    local dest_verify=0
    /usr/bin/codesign --verify --deep --strict "$dest_app" 2>/dev/null || dest_verify=$?
    if [[ "$dest_verify" != 0 ]]; then
        dev_app_die "staged bundle ${dest_app} fails codesign --verify (${dest_verify}); macOS will not give it key focus and it will accept no keyboard input" || return 1
    fi

    printf '%s\n' "${dest_app}/Contents/MacOS/${dest_executable}"
}

# Create the dev home if it does not exist: a config file the dev build owns,
# and the directory the workspace state and automation dropbox land in.
#
# The config is NOT seeded from yours. Copying it would make the first dev run
# look right and every later one stale, and Cockpit WRITES to its config file
# (the settings surface persists a theme choice through
# `Model.writeConfigTheme`), so a dev build pointed at your real file is a dev
# build that can edit it. Point `--config` at any file when you want a specific
# one, including your own.
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

# Launch a staged dev build against a dev home, from inside that home so the
# automation dropbox is the dev home's own. Trailing arguments are extra
# `KEY=value` environment entries. Prints nothing; sets DEV_APP_PID.
#
# `env -C` rather than a `cd` in the caller: the CWD has to move for the child
# only. A script that chdirs into the dev home for the launch and forgets to
# come back would resolve every later relative path -- its own log files, a
# `zig build` -- against the dev home instead of the repo.
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

# Wait until `pgrep -x <name>` reports exactly the pid we launched.
#
# `-x` (exact process name) rather than `-f`, which self-matches the shell
# running it and can never leave a wait loop. Returning the pid ALSO answers
# "is anyone else's instance running under this name", which is the question
# phux-cockpit-2ml.10 is about: a second candidate here means every later
# name-based activation is a coin flip.
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
