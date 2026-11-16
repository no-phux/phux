# shellcheck shell=bash
# Sourced by test-install.sh after its installing section: the installer's
# last line and the shell profile it may edit. Uses install_to and fail.
[[ $HOME == "$TMP"/* ]] || { echo "source this from scripts/test-install.sh" >&2; exit 1; }

# PATH: the plain line when phux is found, a remedy when shadowed, and one
# idempotent profile line when the directory is off PATH.
rm -rf "$HOME" && mkdir "$HOME"
mkdir -p "$TMP/on-path" "$TMP/shadow"
on="$(cd "$TMP/on-path" && pwd -P)"
out="$(TEST_PATH="$on:/usr/bin:/bin" install_to "$on" 2>/dev/null)"
grep -Fxq 'phux is installed. Run `phux` to start.' <<<"$out" || fail "on PATH: $out"
printf '#!/bin/sh\n' > "$TMP/shadow/phux"
chmod 755 "$TMP/shadow/phux"
out="$(TEST_PATH="$TMP/shadow:$on:/usr/bin:/bin" install_to "$on" 2>/dev/null)"
grep -Fq "but $TMP/shadow/phux comes first on PATH. Run $on/phux" <<<"$out" || fail "shadowed: $out"
[[ ! -e $HOME/.zshrc ]] || fail "an on-PATH install edited the profile"
mkdir -p "$TMP/off-path"
off="$(cd "$TMP/off-path" && pwd -P)"
out="$(install_to "$off" 2>/dev/null)"
grep -Fxq 'phux is installed. Open a new terminal and run `phux` (~/.zshrc now adds it to PATH).' <<<"$out" \
  || fail "off PATH: $out"
install_to "$off" >/dev/null 2>&1
[[ $(grep -cFx "export PATH=$off:\"\$PATH\"" "$HOME/.zshrc") -eq 1 ]] || fail "zsh profile line"
SHELL=/usr/bin/fish install_to "$off" >/dev/null 2>&1
grep -Fxq "fish_add_path -g $off" "$HOME/.config/fish/conf.d/phux.fish" || fail "fish profile line"
rm -rf "$HOME/.zshrc"
out="$(PHUX_NO_MODIFY_PATH=1 install_to "$off" 2>/dev/null)"
grep -Fq "Add it to PATH with export PATH=$off:" <<<"$out" || fail "opted out: $out"
install_to "$off" --no-modify-path >/dev/null 2>&1
[[ ! -e $HOME/.zshrc ]] || fail "an opted-out install edited the profile"
