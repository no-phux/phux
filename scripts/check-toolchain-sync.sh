#!/usr/bin/env bash
# Guard the native manifests, the Mise and Nix environments, CI, and containers
# from quietly drifting apart. The authoritative Rust and Zig inputs stay where
# their native consumers require them: rust-toolchain.toml and zig-toolchain.json.
#
# This is a STATIC check: it reads files, so it runs on a CI checkout with no
# Nix, no Mise and no compilers. `just toolchain-parity` is the complementary
# runtime check that actually resolves both environments and compares binaries.
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/dev-toolchain.sh
source "$ROOT/scripts/lib/dev-toolchain.sh"
MISE="$ROOT/mise.toml"
FLAKE="$ROOT/flake.nix"

failures=0

fail() {
    printf 'toolchain sync: %s\n' "$1" >&2
    failures=$((failures + 1))
}

mise_value() {
    sed -n -E "s/^$1[[:space:]]*=[[:space:]]*\"([^\"]+)\".*/\1/p" "$MISE"
}

rust_version="$RUST_CHANNEL"
rust_msrv="${rust_version%.*}"
zig_version="$ZIG_VERSION"
node_version="$(mise_value node)"
bun_version="$(mise_value bun)"

[[ "$(mise_value rust)" == "$rust_version" ]] || fail "mise Rust must be $rust_version"
[[ "$(mise_value zig)" == "$zig_version" ]] || fail "mise Zig must be $zig_version"
[[ "$node_version" =~ ^[0-9]+$ ]] || fail "mise Node must use a major version"
[[ "$bun_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "mise Bun must use an exact release"

# The Nix shell must resolve the same compilers and runtimes as the Mise path.
# Nothing checked this before, and the two had already parted: nixpkgs carried
# Bun 1.3.13 while mise.toml, `@types/bun` and the site's production builder
# were all on 1.4.0, so the Nix path type-checked docs/site against a runtime
# nobody ships. These are string checks against flake.nix rather than an
# evaluation, so they cost nothing and run without Nix installed.
#
# Rust needs no check here: the flake reads rust-toolchain.toml directly.
zig_attr="zig_$(printf '%s' "${zig_version%.*}" | tr '.' '_')"
grep -Fq "pkgs.$zig_attr" "$FLAKE" || fail "flake.nix must use pkgs.$zig_attr for Zig $zig_version"
grep -Fq "pkgs.nodejs_$node_version" "$FLAKE" ||
    fail "flake.nix must use pkgs.nodejs_$node_version for Node $node_version"

# Bun is pinned by construction: the flake reads mise.toml, so the two cannot
# hold different versions. What CAN rot is the digest table the pinned build
# needs, so require an entry for whatever version mise.toml now names.
grep -Fq 'miseTools.bun' "$FLAKE" || fail 'flake.nix must read the Bun pin from mise.toml (bunVersion = miseTools.bun)'
grep -Eq "^[[:space:]]*\"$bun_version\" = \{" "$FLAKE" ||
    fail "flake.nix bunDigests has no entry for Bun $bun_version; add the per-system hashes (see the comment there)"

while IFS=: read -r file line; do
    version="$(printf '%s\n' "$line" | sed -n -E 's/.*"([0-9]+\.[0-9]+)".*/\1/p')"
    [[ "$version" == "$rust_msrv" ]] || fail "$file rust-version must be $rust_msrv"
done < <(grep -H 'rust-version = "[0-9]' "$ROOT"/Cargo.toml "$ROOT"/clients/*/Cargo.toml "$ROOT"/docs/site/edge/Cargo.toml)
grep -Fq "msrv = \"$rust_msrv\"" "$ROOT/clippy.toml" || fail "clippy MSRV must be $rust_msrv"

bindgen_version="$(sed -n -E 's/^wasm-bindgen = "=([^"]+)"/\1/p' "$ROOT/clients/phux-web/Cargo.toml")"
[[ -n "$bindgen_version" ]] || fail "phux-web must pin wasm-bindgen"
for manifest in "$ROOT"/clients/phux-web/Cargo.toml "$ROOT"/clients/phux-vt-web/Cargo.toml "$ROOT"/docs/site/edge/Cargo.toml; do
    grep -Fq "wasm-bindgen = \"=$bindgen_version\"" "$manifest" || fail "$(basename "$(dirname "$manifest")") must pin wasm-bindgen $bindgen_version"
done
grep -Fq "wasm-bindgen-cli --version $bindgen_version" "$ROOT/docs/SETUP.md" || fail "SETUP.md must name wasm-bindgen-cli $bindgen_version"

grep -Eq "rust:${rust_version}-bookworm@sha256:[0-9a-f]{64}" "$ROOT/docs/site/worker/Dockerfile" || fail "site builder must use Rust $rust_version"
grep -Eq "oven/bun:${bun_version}@sha256:[0-9a-f]{64}" "$ROOT/docs/site/worker/Dockerfile" || fail "site builder must use Bun $bun_version"

while IFS=: read -r file line; do
    version="$(printf '%s\n' "$line" | sed -n -E 's/.*node-version: ([0-9]+).*/\1/p')"
    [[ "$version" == "$node_version" ]] || fail "$file Node version must be $node_version"
done < <(grep -H 'node-version: [0-9]' "$ROOT"/.github/workflows/*.yml)

if [[ "$failures" -ne 0 ]]; then
    exit 1
fi

printf 'toolchain sync passed (Rust %s, Zig %s, Node %s, Bun %s)\n' "$rust_version" "$zig_version" "$node_version" "$bun_version"
