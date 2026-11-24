#!/usr/bin/env bash
# Cargo target runner: strip inherited phux state from every test process.
#
# A shell inside a production phux pane exports PHUX_SOCKET, PHUX_TERMINAL_ID,
# and sometimes PHUX_WS_TOKENS and the TLS paths. Those reach anything Cargo
# starts, so a bare `cargo nextest run` or `cargo test` in that pane would
# otherwise aim its tests at the live server. `.cargo/config.toml` installs
# this script as the runner for all Unix targets, which is the one hook that
# covers both test drivers and that can remove a variable, not just set it.
# Nextest setup scripts cannot unset, and per-suite helpers cover one suite.
#
# Removal is the default: a new variable is scrubbed without anyone listing it.
# keep_phux_var names the test knobs, none of which addresses a server. They
# are read by tests, recipes set them, and a pane does not export them.
#
# Cargo also uses a target runner for `cargo run`, which is left alone: dev
# recipes set PHUX_* on purpose there, and the dev/production guards in
# phux-config-core cover it. Test binaries are the only executables Cargo
# places directly in a `deps` directory.
#
# PHUX_SCRUB_CANARY is planted by `.cargo/config.toml` [env]. Its absence in
# crates/phux-config-core/tests/test_env_runner.rs proves this runner ran.
#
# `--all` scrubs for whatever command follows, for test entrypoints that do
# not go through Cargo (just recipes running example scripts).

keep_phux_var() {
    case "$1" in
        PHUX_AUTO_SPAWN_EXIT_AFTER_IDLE | PHUX_NO_UPDATE_CHECK) return 0 ;;
        PHUX_TEST_* | PHUX_CAPTURE_* | PHUX_UPDATE_* | PHUX_PERF_* | PHUX_E2E_*) return 0 ;;
        PHUX_DECODE_FUZZ_* | PHUX_GLASS_PREVIEW_* | PHUX_PIXELS_DUMP) return 0 ;;
        PHUX_GALLERY_OWNER | PHUX_LIVE_PATH_TABLE_ONLY | PHUX_TRACE_DIR | PHUX_WSL_DISTRO) return 0 ;;
        PHUX_DOGFOOD_REAL_AGENTS) return 0 ;;
        PHUX_DESKTOP_DEV_SOCKET) return 1 ;;
        PHUX_DESKTOP_*) return 0 ;;
    esac
    return 1
}

scrub=
if [[ "${1:-}" == --all ]]; then
    shift
    scrub=1
else
    dir="${1%/*}"
    [[ "${dir##*/}" == deps ]] && scrub=1
    unset dir
fi

# macOS strips DYLD_* on the way through the system `env` and `sh`. Nextest
# saves copies for exactly this case; put them back.
for name in "${!NEXTEST_DYLD_@}" "${!NEXTEST_LD_@}"; do
    export "${name#NEXTEST_}=${!name}"
done
unset name

if [[ -n "$scrub" ]]; then
    for name in "${!PHUX_@}"; do
        keep_phux_var "$name" || unset "$name"
    done
    unset name
fi
unset scrub

exec "$@"
