#!/usr/bin/env bash
# Prove the packer is the release tarball, and that install.sh accepts it.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

INSTALLER_SH="$(command -v dash || echo /bin/sh)"
VERSION=v9.8.7
TARGET=x86_64-unknown-linux-gnu
STAGE="phux-${VERSION}-${TARGET}"

write_curl() {
  cat > "$1" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
url=""
out=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    -o) out=$2; shift 2 ;;
    -fsSL|-fsSLI|-f|-s|-S|-L|-I|-q) shift ;;
    -w) shift 2 ;;
    http*) url=$1; shift ;;
    *) shift ;;
  esac
done
case "$url" in
  *.sha256) src="$INSTALL_FIXTURE/${STAGE}.tar.gz.sha256" ;;
  *) src="$INSTALL_FIXTURE/${STAGE}.tar.gz" ;;
esac
cp "$src" "$out"
EOF
  chmod 755 "$1"
}

members() {
  tar -tzf "$1" | sed 's|/$||' | grep -v '^$' | sort -u
}

assert_current_members() {
  local archive=$1
  local got expected
  got="$(members "$archive")"
  expected="$(printf '%s\n' \
    "$STAGE" \
    "$STAGE/phux" \
    "$STAGE/phux-mcp" \
    "$STAGE/README.md" \
    "$STAGE/LICENSE" \
    "$STAGE/NOTICE" \
    "$STAGE/THIRD-PARTY-NOTICES.md" | sort)"
  if [[ $got != "$expected" ]]; then
    printf 'packed members drifted from the release contract\n got:\n%s\n expected:\n%s\n' \
      "$got" "$expected" >&2
    exit 1
  fi
}

install_archive() {
  local fixture=$1
  local dest=$2
  mkdir -p "$dest" "$TMP/fake-bin"
  STAGE="$STAGE" write_curl "$TMP/fake-bin/curl"
  PATH="$TMP/fake-bin:/usr/bin:/bin" INSTALL_FIXTURE="$fixture" STAGE="$STAGE" \
    "$INSTALLER_SH" "$ROOT/scripts/install.sh" --version "$VERSION" --os linux --arch x86_64 \
      --install-dir "$dest" >/dev/null
  grep -Fxq ok "$dest/phux"
  grep -Fxq ok "$dest/phux-mcp"
}

BINS="$TMP/bins"
mkdir -p "$BINS"
printf 'ok\n' > "$BINS/phux"
printf 'ok\n' > "$BINS/phux-mcp"
chmod 755 "$BINS/phux" "$BINS/phux-mcp"

PACKED="$TMP/packed"
mkdir "$PACKED"
bash "$ROOT/scripts/pack-release.sh" \
  --id "$VERSION" --target "$TARGET" --bin-dir "$BINS" --out-dir "$PACKED"
assert_current_members "$PACKED/${STAGE}.tar.gz"
grep -Eq '^[0-9a-f]{64}  '"${STAGE}.tar.gz"'$' "$PACKED/${STAGE}.tar.gz.sha256"
install_archive "$PACKED" "$TMP/from-packer"

# A docs-dir missing LICENSE must fail at pack time, not at a stranger's curl | sh.
MISSING="$TMP/missing-docs"
mkdir "$MISSING"
cp "$ROOT/README.md" "$MISSING/README.md"
if bash "$ROOT/scripts/pack-release.sh" \
  --id "$VERSION" --target "$TARGET" --bin-dir "$BINS" --out-dir "$TMP/should-fail" \
  --docs-dir "$MISSING" 2>"$TMP/missing.err"; then
  echo "packer accepted a docs-dir with no LICENSE" >&2
  exit 1
fi
grep -Fq 'missing release doc' "$TMP/missing.err"

# Historical dual-license tarball (<=v0.35.0) must still install.
OLD="$TMP/old"
mkdir -p "$OLD/$STAGE"
printf 'ok\n' > "$OLD/$STAGE/phux"
printf 'ok\n' > "$OLD/$STAGE/phux-mcp"
chmod 755 "$OLD/$STAGE/phux" "$OLD/$STAGE/phux-mcp"
printf 'readme\n' > "$OLD/$STAGE/README.md"
printf 'mit\n' > "$OLD/$STAGE/LICENSE-MIT"
printf 'apache\n' > "$OLD/$STAGE/LICENSE-APACHE"
tar -czf "$OLD/${STAGE}.tar.gz" -C "$OLD" "$STAGE"
(
  cd "$OLD"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "${STAGE}.tar.gz" > "${STAGE}.tar.gz.sha256"
  else
    shasum -a 256 "${STAGE}.tar.gz" > "${STAGE}.tar.gz.sha256"
  fi
)
install_archive "$OLD" "$TMP/from-old"

# A stranger file is still refused.
HOSTILE="$TMP/hostile"
mkdir -p "$HOSTILE/$STAGE"
printf 'ok\n' > "$HOSTILE/$STAGE/phux"
printf 'ok\n' > "$HOSTILE/$STAGE/phux-mcp"
chmod 755 "$HOSTILE/$STAGE/phux" "$HOSTILE/$STAGE/phux-mcp"
printf 'readme\n' > "$HOSTILE/$STAGE/README.md"
printf 'x\n' > "$HOSTILE/$STAGE/LICENSE"
printf 'pwn\n' > "$HOSTILE/$STAGE/payload.sh"
tar -czf "$HOSTILE/${STAGE}.tar.gz" -C "$HOSTILE" "$STAGE"
(
  cd "$HOSTILE"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "${STAGE}.tar.gz" > "${STAGE}.tar.gz.sha256"
  else
    shasum -a 256 "${STAGE}.tar.gz" > "${STAGE}.tar.gz.sha256"
  fi
)
if install_archive "$HOSTILE" "$TMP/from-hostile" 2>"$TMP/hostile.err"; then
  echo "installer accepted an unexpected archive member" >&2
  exit 1
fi
grep -Fq 'unexpected archive member' "$TMP/hostile.err"

echo "packer contract tests passed"
