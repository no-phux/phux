#!/usr/bin/env bash
# Black-box docs gate fixtures: only a disposable tree, no compiler or server.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
scratch="$(mktemp -d "${TMPDIR:-/tmp}/phux-docs-test.XXXXXX")"
# Resolve /var -> /private/var on macOS before exact component matching.
scratch="$(cd "$scratch" && pwd -P)"
trap 'rm -rf "$scratch"' EXIT
repo="$scratch/repo"
mkdir -p "$repo/scripts/lib" "$repo/docs/Parent" "$repo/just"
cp "$root/scripts/check-docs.sh" "$repo/scripts/"
cp "$root/scripts/lib/output.sh" "$repo/scripts/lib/"
# A Git fixture proves collection ignores untracked build/cache trees normally
# excluded by Git, but still checks new authored docs.
git -C "$repo" init -q
printf 'ignored/\n' >"$repo/.gitignore"
printf 'check:\nalias verify := check\n' >"$repo/justfile"
printf 'scoped:\n' >"$repo/just/docs.just"
mkdir -p "$repo/docs/reference"
printf '# CLI\n\n## `phux agent`\n\n## `phux agent wait`\n\nUsage: phux agent <SUBCOMMAND>\n\n## `phux agent show`\n' >"$repo/docs/reference/cli.md"
printf '# Target\n\n## Build cache (mbx)\n\n## Repeat\n\n## Repeat\n\n<a id="explicit"></a>\n' >"$repo/docs/Parent/Leaf.md"
expect_pass() {
    if ! "$BASH" "$repo/scripts/check-docs.sh" "$@" >"$scratch/output" 2>&1; then
        printf 'unexpected failure: %s\n' "$*" >&2
        head -80 "$scratch/output" >&2
        exit 1
    fi
}
expect_fail() {
    local expected="$1" status=0
    shift
    "$BASH" "$repo/scripts/check-docs.sh" "$@" >"$scratch/output" 2>&1 || status=$?
    [[ "$status" == 1 ]] || { printf 'expected violation, got %s\n' "$status" >&2; exit 1; }
    grep -Fq -- "$expected" "$scratch/output" || { head -80 "$scratch/output" >&2; exit 1; }
}
# Valid .., root-relative paths, directory links, query stripping, duplicate and
# explicit anchors; code spans/fences and external links must not be followed.
cat >"$repo/docs/guide.md" <<'MD'
# Guide
[relative](./Parent/../Parent/Leaf.md#build-cache-mbx)
[root](/docs/Parent/Leaf.md?view=1#repeat-1)
[directory](./Parent/)
[explicit](./Parent/Leaf.md#explicit)
[self](#guide)
[web](https://example.invalid/missing)
`[inline code](missing.md)`
```md
[code block](missing.md)
```
```sh
phux agent wait @7
just check
just verify
just scoped
```
MD
expect_pass
mkdir -p "$repo/ignored"
printf '[ignored](missing.md)\n' >"$repo/ignored/bad.md"
expect_pass
printf '[wrong parent](./parent/Leaf.md)\n' >>"$repo/docs/guide.md"
expect_fail 'parent/Leaf.md does not exist' --only=links
# Wrong case cannot be hidden by a later .. component.
printf '[wrong parent](./parent/../Parent/Leaf.md)\n' >"$repo/docs/guide.md"
expect_fail 'parent/../Parent/Leaf.md does not exist' --only=links
printf '[wrong leaf](./Parent/leaf.md)\n' >"$repo/docs/guide.md"
expect_fail 'Parent/leaf.md does not exist' --only=links
printf '[missing](./missing.md)\n' >"$repo/docs/guide.md"
expect_fail 'missing.md does not exist' --only=links
printf '[anchor](./Parent/Leaf.md#missing)\n' >"$repo/docs/guide.md"
expect_fail 'has no heading #missing' --only=links
printf '```sh\nphux gone\n```\n' >"$repo/docs/guide.md"
expect_fail '`phux gone` is not a command' --only=commands
printf '```sh\nphux agent gone\n```\n' >"$repo/docs/guide.md"
expect_fail '`phux agent gone` is not a command' --only=commands
printf '```sh\njust retired\n```\n' >"$repo/docs/guide.md"
expect_fail '`just retired` is not a recipe' --only=commands
mkdir -p "$repo/crates/phux-fixture"
printf '[package]\n' >"$repo/crates/phux-fixture/Cargo.toml"
printf '# Architecture\n' >"$repo/docs/architecture.md"
expect_fail 'crates/phux-fixture is not in the crate table' --only=crates
printf '\n## Crates\n\nA prose mention of `phux-fixture` is not an inventory row.\n' >>"$repo/docs/architecture.md"
expect_fail 'crates/phux-fixture is not in the crate table' --only=crates
printf '\n| Crate | Role |\n|---|---|\n| `phux-fixture` | Fixture |\n' >>"$repo/docs/architecture.md"
expect_pass --only=crates
printf '\nThe standalone client workspace includes `phux-gpui`.\n' >>"$repo/docs/architecture.md"
expect_pass --only=crates
printf '\n| `phux-missing` | Stale root crate |\n' >>"$repo/docs/architecture.md"
expect_fail 'phux-missing is not a crate in this tree' --only=crates
printf 'docs fixtures: all cases passed\n'
