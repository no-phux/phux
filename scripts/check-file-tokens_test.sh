#!/usr/bin/env bash
# Hermetic staged-blob coverage, including real conflicted and clean Git merges.
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
GATE="${ROOT}/scripts/check-file-tokens.sh"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/check-file-tokens-test.XXXXXX")"
trap 'rm -rf -- "$WORK"' EXIT
FAILURES=0
REPO=''
mkdir "${WORK}/template"

ok() { printf '  ok: %s\n' "$*"; }
bad() { printf '  FAILED: %s\n' "$*" >&2; FAILURES=$((FAILURES + 1)); }

g() {
    git -C "$REPO" -c user.name=t -c user.email=t@t \
        -c commit.gpgsign=false -c rerere.enabled=false -c core.autocrlf=false "$@"
}

gate() { (cd "$REPO" && "$BASH" "$GATE" 2>"${WORK}/gate.err"); }
write_bytes() { head -c "$2" /dev/zero | tr '\0' 'x' >"$1"; }

new_repo() {
    REPO="${WORK}/$1"
    mkdir "$REPO"
    g init -q -b main --template="${WORK}/template"
    printf 'seed\n' >"${REPO}/README.md"
    g add README.md
    g commit -q -m seed
}

expect_gate() {
    local expected="$1" label="$2" actual
    if gate; then actual=0; else actual=$?; fi
    if [[ "$actual" == "$expected" ]]; then
        ok "$label"
    else
        bad "$label (expected exit $expected, got $actual): $(<"${WORK}/gate.err")"
    fi
}

expect_named() {
    if grep -Fq "  $1  ~" "${WORK}/gate.err"; then
        ok "rejection names $1"
    else
        bad "rejection does not name $1"
    fi
}

expect_not_named() {
    if grep -Fq "  $1  ~" "${WORK}/gate.err"; then
        bad "inherited $1 appears in rejection"
    else
        ok "inherited $1 is not rejected"
    fi
}

assert_same_blob() {
    if [[ "$(g rev-parse ":$1")" == "$(g rev-parse "$2:$1")" ]]; then
        ok "fixture: $1 matches $2"
    else
        bad "fixture: $1 does not match $2"
    fi
}

assert_novel_blob() {
    local parent
    for parent in HEAD MERGE_HEAD; do
        if [[ "$(g rev-parse ":$1")" == "$(g rev-parse "$parent:$1")" ]]; then
            bad "fixture: $1 unexpectedly matches $parent"
        fi
    done
}

# Branches deliberately conflict on the last line of an oversized text file.
conflicting_merge() {
    new_repo "$1"
    write_bytes "${REPO}/big.md" 25000
    printf '\nbase\n' >>"${REPO}/big.md"
    g add big.md
    g commit -q -m base
    g checkout -q -b feature
    g show HEAD:big.md >"${WORK}/base"
    { head -c 25001 "${WORK}/base"; printf 'feature\n'; } >"${REPO}/big.md"
    g add big.md
    g commit -q -m feature
    g checkout -q main
    { head -c 25001 "${WORK}/base"; printf 'main\n'; } >"${REPO}/big.md"
    g add big.md
    g commit -q -m main
    if g merge --no-ff --no-commit feature >"${WORK}/merge.log" 2>&1; then
        bad 'fixture: expected a merge conflict'
    fi
    g rev-parse --verify MERGE_HEAD >/dev/null
    if [[ -z "$(g ls-files -u -- big.md)" ]]; then
        bad 'fixture: big.md must be unmerged'
    fi
}

resolve_from() {
    g show "$1:big.md" >"${REPO}/big.md"
    g add big.md
    assert_same_blob big.md "$1"
}

test_ordinary_commits() {
    new_repo ordinary
    write_bytes "${REPO}/big.md" 20000
    g add big.md
    expect_gate 0 'ordinary: exact 20000-byte boundary passes'
    printf x >>"${REPO}/big.md"
    expect_gate 0 'ordinary: unstaged oversized bytes do not affect staged boundary'
    g add big.md
    expect_gate 1 'ordinary: 20001-byte addition fails'
    expect_named big.md
    write_bytes "${REPO}/big.md" 20000
    expect_gate 1 'ordinary: unstaged shrink does not hide staged oversized bytes'
    g add big.md
    g commit -q -m boundary
    printf x >>"${REPO}/big.md"
    g add big.md
    expect_gate 1 'ordinary: 20001-byte modification fails'
    g commit -q -m oversized-fixture
    printf x >>"${REPO}/big.md"
    g add big.md
    expect_gate 1 'ordinary: existing oversized HEAD does not grant an exemption'
}

test_conflict_resolutions() {
    conflicting_merge conflicts
    resolve_from MERGE_HEAD
    expect_gate 0 'merge: exact oversized MERGE_HEAD blob passes'
    printf x >>"${REPO}/big.md"
    expect_gate 0 'merge: unstaged edit of inherited blob is ignored'
    g add big.md
    expect_gate 1 'merge: one-byte edit of inherited oversized blob fails'
    expect_named big.md
    g show MERGE_HEAD:big.md >"${REPO}/big.md"
    expect_gate 1 'merge: unstaged parent content cannot hide staged novel bytes'
    resolve_from HEAD
    # Content inherited at another path is not grandfathered.
    g show MERGE_HEAD:big.md >"${REPO}/other.md"
    g add other.md
    expect_gate 1 'merge: same blob at a different path fails'
    expect_named other.md
    expect_not_named big.md
    g rm -q --cached other.md
    rm "${REPO}/other.md"
    { head -c 25001 "${WORK}/base"; printf 'main\nfeature\n'; } >"${REPO}/big.md"
    g add big.md
    assert_novel_blob big.md
    expect_gate 1 'merge: novel manual resolution combining parents fails'
    expect_named big.md
    write_bytes "${REPO}/big.md" 20000
    g add big.md
    expect_gate 0 'merge: novel resolution at exact boundary passes'
    resolve_from MERGE_HEAD
    write_bytes "${REPO}/born-in-merge.md" 20001
    g add born-in-merge.md
    expect_gate 1 'merge: new oversized file fails beside inherited file'
    expect_named born-in-merge.md
    expect_not_named big.md
}

test_clean_inheritance() {
    new_repo clean-inheritance
    g checkout -q -b feature
    write_bytes "${REPO}/big.md" 25000
    g add big.md
    g commit -q -m feature
    g checkout -q main
    printf 'main\n' >>"${REPO}/README.md"
    g add README.md
    g commit -q -m main
    g merge -q --no-ff --no-commit feature >"${WORK}/merge.log" 2>&1
    g rev-parse --verify MERGE_HEAD >/dev/null
    assert_same_blob big.md MERGE_HEAD
    expect_gate 0 'clean merge: oversized blob present in only one parent passes'
    printf x >>"${REPO}/big.md"
    g add big.md
    expect_gate 1 'clean merge: one-byte edit of sole oversized parent fails'
}

# Clean merges can create oversized blobs from either legal or large parents.
clean_combination() {
    local name="$1" middle_bytes="$2" parent_result="$3"
    new_repo "$name"
    write_bytes "${WORK}/middle" "$middle_bytes"
    { printf 'start\n'; cat "${WORK}/middle"; printf '\nend\n'; } >"${REPO}/combined.md"
    g add combined.md
    g commit -q -m base
    g checkout -q -b feature
    { printf 'feature addition\n'; g show HEAD:combined.md; } >"${REPO}/combined.md"
    g add combined.md
    expect_gate "$parent_result" "$name: feature parent budget check"
    g commit -q -m feature
    g checkout -q main
    printf 'main addition\n' >>"${REPO}/combined.md"
    g add combined.md
    expect_gate "$parent_result" "$name: main parent budget check"
    g commit -q -m main
    g merge -q --no-ff --no-commit feature >"${WORK}/merge.log" 2>&1
    g rev-parse --verify MERGE_HEAD >/dev/null
    assert_novel_blob combined.md
    expect_gate 1 "$name: clean automatic combination over budget fails"
    expect_named combined.md
}

# A mode-only staged change exercises inheritance from HEAD, not MERGE_HEAD.
test_head_inheritance() {
    new_repo head-parent
    write_bytes "${REPO}/big.md" 25000
    g add big.md
    g commit -q -m base
    g checkout -q -b feature
    g rm -q big.md
    g commit -q -m delete
    g checkout -q main
    printf x >>"${REPO}/big.md"
    g add big.md
    g commit -q -m modify
    if g merge --no-ff --no-commit feature >"${WORK}/merge.log" 2>&1; then
        bad 'fixture: expected modify/delete conflict'
    fi
    g rev-parse --verify MERGE_HEAD >/dev/null
    resolve_from HEAD
    g update-index --chmod=+x big.md
    expect_gate 0 'merge: HEAD blob with staged mode change passes'
    g merge --quit
    expect_gate 1 'ordinary: same oversized HEAD blob without merge metadata fails'
}

test_merge_metadata() {
    local metadata parent invalid
    conflicting_merge metadata
    resolve_from MERGE_HEAD
    metadata="$(g rev-parse --git-path MERGE_HEAD)"
    [[ "$metadata" = /* ]] || metadata="${REPO}/${metadata}"
    parent="$(g rev-parse MERGE_HEAD)"
    # Only commit identity matters; a branch ref is not MERGE_HEAD metadata.
    for invalid in '' feature "${parent:0:12}" \
        0000000000000000000000000000000000000000 \
        "$(g rev-parse MERGE_HEAD:big.md)" "$(g rev-parse 'MERGE_HEAD^{tree}')"; do
        printf '%s\n' "$invalid" >"$metadata"
        expect_gate 1 "metadata: invalid entry '$invalid' grants no exemption"
    done
    : >"$metadata"
    expect_gate 1 'metadata: empty MERGE_HEAD grants no exemption'
    printf '%s\ninvalid\n' "$parent" >"$metadata"
    expect_gate 1 'metadata: one invalid entry invalidates all parents'
    rm "$metadata"
    expect_gate 1 'metadata: absent MERGE_HEAD grants no exemption'
    printf '%s' "$parent" >"$metadata"
    expect_gate 0 'metadata: valid commit ID without final newline passes'
    resolve_from MERGE_HEAD
    g branch -D feature >/dev/null
    expect_gate 0 'metadata: parent needs no branch name'
}

test_linked_worktree() {
    local source metadata
    conflicting_merge linked-source
    g merge --abort
    source="$REPO"
    g worktree add -q -b linked "${WORK}/linked" main
    REPO="${WORK}/linked"
    if g merge --no-ff --no-commit feature >"${WORK}/merge.log" 2>&1; then
        bad 'fixture: linked-worktree merge must conflict'
    fi
    metadata="$(g rev-parse --git-path MERGE_HEAD)"
    if [[ ! -f "${REPO}/.git" || ! -f "$metadata" ]]; then
        bad 'fixture: expected linked worktree and resolved MERGE_HEAD'
    fi
    resolve_from MERGE_HEAD
    expect_gate 0 'linked worktree: exact inherited oversized blob passes'
    printf x >>"${REPO}/big.md"
    g add big.md
    expect_gate 1 'linked worktree: edited inherited blob fails'
    g merge --abort
    REPO="$source"
    g worktree remove "${WORK}/linked"
}

test_exemptions() {
    local file header
    new_repo exemptions
    for file in Cargo.lock bun.lockb package-lock.json pnpm-lock.yaml \
        dependency.lock.json go.sum Package.resolved npm-shrinkwrap.json gradle.lockfile; do
        write_bytes "${REPO}/$file" 25000
    done
    { printf '\0'; head -c 25000 /dev/zero; } >"${REPO}/binary.dat"
    for header in '@generated' 'Code generated' 'auto-generated' 'DO NOT EDIT'; do
        file="generated-${header// /-}.md"
        { printf '%s\n' "$header"; head -c 25000 /dev/zero | tr '\0' 'x'; } >"${REPO}/$file"
    done
    write_bytes "${REPO}/generated.md" 25000
    write_bytes "${REPO}/vendored.md" 25000
    printf 'generated.md linguist-generated=true\nvendored.md linguist-vendored\n' >"${REPO}/.gitattributes"
    g add -A
    # Attribute exemptions, like content checks, must read the index.
    : >"${REPO}/.gitattributes"
    expect_gate 0 'ordinary: lockfile, binary, header and cached attribute exemptions pass'
    g commit -q -m exempt-fixtures
    g checkout -q -b feature
    printf 'feature\n' >"${REPO}/feature.md"
    g add feature.md
    g commit -q -m feature
    g checkout -q main
    printf 'main\n' >>"${REPO}/README.md"
    g add README.md
    g commit -q -m main
    g merge -q --no-ff --no-commit feature >"${WORK}/merge.log" 2>&1
    # New exempt files, not just unchanged parent blobs, still pass in a merge.
    write_bytes "${REPO}/new.lock" 25000
    { printf 'Code generated\n'; head -c 25000 /dev/zero | tr '\0' 'x'; } >"${REPO}/new-generated.md"
    g add new.lock new-generated.md
    expect_gate 0 'merge: newly staged existing exemption types still pass'
}

# The producer must drain blobs larger than a pipe buffer, but only the first
# 2048 bytes may grant the generated-header exemption.
test_generated_prefix() {
    new_repo generated-prefix
    { printf 'Code generated\n'; head -c 1048576 /dev/zero | tr '\0' 'x'; } >"${REPO}/early.md"
    { head -c 2034 /dev/zero | tr '\0' 'x'; printf 'Code generated'; head -c 1048576 /dev/zero | tr '\0' 'x'; } >"${REPO}/boundary.md"
    g add early.md boundary.md
    expect_gate 0 'generated headers: large blobs drain and a marker ending at byte 2048 passes'

    { head -c 2048 /dev/zero | tr '\0' 'x'; printf 'Code generated\n'; head -c 1048576 /dev/zero | tr '\0' 'x'; } >"${REPO}/late.md"
    write_bytes "${REPO}/plain.md" 1048576
    g add late.md plain.md
    expect_gate 1 'generated headers: a later marker and plain authored content remain subject to budget'
    expect_named late.md
    expect_named plain.md
    expect_not_named early.md
    expect_not_named boundary.md
}

test_ordinary_commits
test_conflict_resolutions
test_clean_inheritance
clean_combination clean-legal 19970 0
clean_combination clean-oversized 25000 1
test_head_inheritance
test_merge_metadata
test_linked_worktree
test_exemptions
test_generated_prefix

if ((FAILURES > 0)); then
    printf 'check-file-tokens_test: %d failure(s)\n' "$FAILURES" >&2
    exit 1
fi
printf 'check-file-tokens_test: ok\n'
