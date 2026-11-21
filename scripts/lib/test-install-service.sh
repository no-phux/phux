# shellcheck shell=bash
# Sourced by test-install.sh after its installing section: install.sh on Linux
# offering `phux service install`, installing over a running server and handing
# an older one over. Uses its package, install_to, no_leftovers and fail.
# Its HOME is a temp dir; refuse anything else (phux-gh85y).
[[ $HOME == "$TMP"/* ]] || { echo "source this from scripts/test-install.sh" >&2; exit 1; }
# --- keeping the server running (Linux) -----------------------------------------

# A fake uname says Linux and a fake systemctl stands in for the user manager;
# the installed phux records what it was asked. Nothing reaches real systemd.
mkdir -p "$TMP/svc-bin"
printf '#!/bin/sh\necho Linux\n' > "$TMP/svc-bin/uname"
printf '#!/bin/sh\nexit "${NO_SYSTEMD:-0}"\n' > "$TMP/svc-bin/systemctl"
chmod 755 "$TMP/svc-bin/"*
export PHUX_CALLS="$TMP/phux-calls"
svc() {
  : > "$PHUX_CALLS"
  TEST_PATH="$TMP/svc-bin:/usr/bin:/bin" PHUX_NO_SERVICE="${NO_SERVICE:-}" \
    install_to "$TMP/svc" --no-modify-path "$@" >"$TMP/out" 2>"$TMP/err"
}
# No terminal to ask on: install, without a prompt.
svc || fail "service install: $(cat "$TMP/err")"
grep -Fxq 'service install --adopt' "$PHUX_CALLS" || fail "no tty installs the service: $(cat "$PHUX_CALLS")"
if grep -Fq '[Y/n]' "$TMP/err"; then fail "prompted without a terminal"; fi
svc --service && grep -Fxq 'service install --adopt' "$PHUX_CALLS" || fail "--service"
for opt_out in --no-service NO_SERVICE=1; do
  if [[ $opt_out == --* ]]; then svc "$opt_out"; else NO_SERVICE=1 svc; fi
  if grep -q '^service' "$PHUX_CALLS"; then fail "$opt_out still ran: $(cat "$PHUX_CALLS")"; fi
done
# No user manager (a container, WSL without systemd): skip, quietly unless asked.
NO_SYSTEMD=1 svc && ! grep -q '^service' "$PHUX_CALLS" && ! grep -Fq 'systemd' "$TMP/err" || fail "no systemd, default"
NO_SYSTEMD=1 svc --service && grep -Fq 'no systemd user manager' "$TMP/err" || fail "no systemd, --service"
# A failed service install never fails the install, and says how to retry.
PHUX_EXIT=1 svc || fail "a failed service install failed the install"
grep -Fq 'retry with:' "$TMP/err" && grep -Fxq 'phux v9.8.7' "$TMP/svc/phux" || fail "service failure: $(cat "$TMP/err")"

# --- installing over a running server -------------------------------------------

# A running phux keeps its installed path: its /proc/<pid>/exe must read
# "<dir>/phux (deleted)", which a server strips back to the new binary, never
# a path in the deleted transaction directory (phux #2785). Linux only.
if [[ -e /proc/self/exe ]]; then
  mkdir "$TMP/live"
  cp "$(command -v bash)" "$TMP/live/phux"
  mkfifo "$TMP/live.fifo"
  "$TMP/live/phux" -c 'read -r _ <"$0"' "$TMP/live.fifo" &
  live=$!
  for _ in {1..50}; do
    [[ $(readlink "/proc/$live/exe") != "$TMP/live/phux" ]] || break
    sleep 0.1
  done
  install_to "$TMP/live" >/dev/null 2>&1 || fail "an install over a running phux"
  exe="$(readlink "/proc/$live/exe")"
  kill "$live"
  wait "$live" 2>/dev/null || true
  [[ $exe == "$TMP/live/phux (deleted)" ]] || fail "the running phux followed its backup to $exe"
  grep -Fxq 'phux v9.8.7' "$TMP/live/phux" || fail "an install over a running phux"
  no_leftovers "$TMP/live" "an install over a running phux"
fi

# --- handing an older server over -----------------------------------------------

# The installed phux reports the running server through `status --json`; an
# older release is handed over with `phux upgrade`, quietly, never fatally.
status_doc() { printf '{\n  "server_version": "%s",\n  "client_version": "9.8.7"\n}' "$1"; }
handover() { PHUX_STATUS="$1" svc --no-service; }
handover "$(status_doc 9.8.6)" || fail "hand-over install: $(cat "$TMP/err")"
grep -Fxq upgrade "$PHUX_CALLS" && [[ ! -s $TMP/err ]] || fail "an older server is handed over quietly: $(cat "$PHUX_CALLS")"
for theirs in 9.8.7 9.10.0; do
  handover "$(status_doc "$theirs")" && grep -Fxq 'status --json' "$PHUX_CALLS" || fail "status for $theirs"
  if grep -Fxq upgrade "$PHUX_CALLS"; then fail "a $theirs server was handed over"; fi
done
handover "" && grep -Fxq 'status --json' "$PHUX_CALLS" || fail "no server"
if grep -Fxq upgrade "$PHUX_CALLS"; then fail "upgraded with no server"; fi
PHUX_UPGRADE_EXIT=2 handover "$(status_doc 9.8.6)" || fail "a refused hand-over failed the install"
grep -Fq 'still 9.8.6; hand it over with:' "$TMP/err" || fail "refused hand-over hint: $(cat "$TMP/err")"
