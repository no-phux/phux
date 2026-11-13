# shellcheck shell=bash
# Sourced by test-install.sh after its installing section: install.sh on Linux
# offering `phux service install`. Uses its package, install_to and fail.
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
  [[ ! -s $PHUX_CALLS ]] || fail "$opt_out still ran: $(cat "$PHUX_CALLS")"
done
# No user manager (a container, WSL without systemd): skip, quietly unless asked.
NO_SYSTEMD=1 svc && [[ ! -s $PHUX_CALLS ]] && ! grep -Fq 'systemd' "$TMP/err" || fail "no systemd, default"
NO_SYSTEMD=1 svc --service && grep -Fq 'no systemd user manager' "$TMP/err" || fail "no systemd, --service"
# A failed service install never fails the install, and says how to retry.
PHUX_EXIT=1 svc || fail "a failed service install failed the install"
grep -Fq 'retry with:' "$TMP/err" && grep -Fxq 'phux v9.8.7' "$TMP/svc/phux" || fail "service failure: $(cat "$TMP/err")"
