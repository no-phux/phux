#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

VERSION=v9.8.7
TARGET=x86_64-unknown-linux-gnu
STAGE="phux-${VERSION}-${TARGET}"
FIXTURE="$TMP/fixture"
FAKE_BIN="$TMP/fake-bin"
mkdir -p "$FIXTURE/$STAGE" "$FAKE_BIN"
printf 'new phux\n' > "$FIXTURE/$STAGE/phux"
printf 'new phux-mcp\n' > "$FIXTURE/$STAGE/phux-mcp"
chmod 755 "$FIXTURE/$STAGE/phux" "$FIXTURE/$STAGE/phux-mcp"
tar -czf "$FIXTURE/$STAGE.tar.gz" -C "$FIXTURE" "$STAGE"
(
  cd "$FIXTURE"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$STAGE.tar.gz" > "$STAGE.tar.gz.sha256"
  else
    shasum -a 256 "$STAGE.tar.gz" > "$STAGE.tar.gz.sha256"
  fi
)

cat > "$FAKE_BIN/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
url=$2
out=$4
case "$url" in
  *.sha256) cp "$INSTALL_FIXTURE/phux-v9.8.7-x86_64-unknown-linux-gnu.tar.gz.sha256" "$out" ;;
  *) cp "$INSTALL_FIXTURE/phux-v9.8.7-x86_64-unknown-linux-gnu.tar.gz" "$out" ;;
esac
EOF
chmod 755 "$FAKE_BIN/curl"

run_install() {
  local install_dir=$1
  local path=$2
  PATH="$FAKE_BIN:$path" INSTALL_FIXTURE="$FIXTURE" \
    bash "$ROOT/scripts/install.sh" --version "$VERSION" --os linux --arch x86_64 \
      --install-dir "$install_dir"
}

ON_PATH="$TMP/on-path"
mkdir "$ON_PATH"
ON_PATH_CANON="$(cd "$ON_PATH" && pwd -P)"
output="$(run_install "$ON_PATH" "$ON_PATH:/usr/bin:/bin")"
grep -Fq 'next: phux' <<<"$output"
if grep -Fq 'PATH remedy:' <<<"$output"; then
  echo "installer printed a PATH remedy for a discoverable destination" >&2
  exit 1
fi
cmp "$FIXTURE/$STAGE/phux" "$ON_PATH/phux"
cmp "$FIXTURE/$STAGE/phux-mcp" "$ON_PATH/phux-mcp"

SHADOW="$TMP/shadow"
mkdir "$SHADOW"
printf '#!/bin/sh\nexit 0\n' > "$SHADOW/phux"
chmod 755 "$SHADOW/phux"
output="$(run_install "$ON_PATH" "$SHADOW:$ON_PATH:/usr/bin:/bin")"
grep -Fq "next: $ON_PATH_CANON/phux" <<<"$output"
grep -Fq 'PATH remedy:' <<<"$output"

OFF_PATH="$TMP/off-path"
output="$(run_install "$OFF_PATH" "/usr/bin:/bin")"
OFF_PATH_CANON="$(cd "$OFF_PATH" && pwd -P)"
grep -Fq "next: $OFF_PATH_CANON/phux" <<<"$output"
grep -Fq "PATH remedy: export PATH=$OFF_PATH_CANON:\"\$PATH\"" <<<"$output"

# Fail only the second publish rename. The EXIT trap must restore both old
# binaries and remove its lock and transaction directory.
cat > "$FAKE_BIN/mv" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
src=${@: -2:1}
dst=${@: -1}
if [[ $src == */.phux-install.*/phux-mcp && $dst == "$FAIL_INSTALL_DIR/phux-mcp" && ! -e $FAIL_MARKER ]]; then
  : > "$FAIL_MARKER"
  exit 1
fi
exec /bin/mv "$@"
EOF
chmod 755 "$FAKE_BIN/mv"

ROLLBACK="$TMP/rollback"
mkdir "$ROLLBACK"
printf 'old phux\n' > "$ROLLBACK/phux"
printf 'old phux-mcp\n' > "$ROLLBACK/phux-mcp"
if PATH="$FAKE_BIN:/usr/bin:/bin" INSTALL_FIXTURE="$FIXTURE" \
  FAIL_INSTALL_DIR="$ROLLBACK" FAIL_MARKER="$TMP/failed-once" \
  bash "$ROOT/scripts/install.sh" --version "$VERSION" --os linux --arch x86_64 \
    --install-dir "$ROLLBACK" >"$TMP/rollback.out" 2>"$TMP/rollback.err"; then
  echo "installer unexpectedly succeeded after forced second publish failure" >&2
  exit 1
fi
grep -Fxq 'old phux' "$ROLLBACK/phux"
grep -Fxq 'old phux-mcp' "$ROLLBACK/phux-mcp"
if find "$ROLLBACK" -maxdepth 1 -name '.phux-install*' -print -quit | grep -q .; then
  echo "installer left transaction artifacts after rollback" >&2
  exit 1
fi

echo "installer transaction tests passed"
