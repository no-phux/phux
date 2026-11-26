#!/usr/bin/env bash
# Hermetic coverage for dev/worktree-prune.sh: a scratch origin and clone
# with one linked worktree per rule, aged past --idle-hours, then a dry run
# (changes nothing) and an applied run (removes only clean worktrees whose
# work landed, and prunes build output only where allowed).
# shellcheck disable=SC2034 # OUT is read through check's eval
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/worktree-prune-test.XXXXXX")"
WORK="$(cd "$WORK" && pwd -P)"
trap 'rm -rf -- "$WORK"' EXIT
FAILURES=0

ok() { printf '  ok: %s\n' "$*"; }
bad() { printf '  FAILED: %s\n' "$*" >&2; FAILURES=$((FAILURES + 1)); }
check() { if eval "$2"; then ok "$1"; else bad "$1"; fi; }

# Stub mbx, which always fails, and gh, which knows one merged PR (#42) and
# the commits listed in $WORK/squashed: the test must not reach a real cache
# or the network.
mkdir -p "$WORK/bin"
printf '#!/bin/sh\nexit 1\n' >"$WORK/bin/mbx"
cat >"$WORK/bin/gh" <<EOF
#!/bin/sh
for sha in \$(cat "$WORK/squashed" 2>/dev/null); do
    case "\$*" in *"\$sha"*) echo 42; exit 0 ;; esac
done
exit 0
EOF
chmod +x "$WORK/bin/mbx" "$WORK/bin/gh"
SAFE_PATH="$WORK/bin:$PATH"

g() { git -c user.name=t -c user.email=t@t -c init.defaultBranch=main "$@"; }

# ------------------------------------------------------------- fixture
# Name the branch explicitly: cloning an empty repository takes the remote's
# unborn HEAD, which follows the host's init.defaultBranch, not ours.
git init -q --bare "$WORK/origin.git"
git -C "$WORK/origin.git" symbolic-ref HEAD refs/heads/main
g clone -q "$WORK/origin.git" "$WORK/repo" 2>/dev/null
REPO="$WORK/repo"
git -C "$REPO" symbolic-ref HEAD refs/heads/main
mkdir -p "$REPO/dev/lib"
cp "$ROOT/dev/worktree-prune.sh" "$REPO/dev/"
cp "$ROOT/dev/lib/worktree-owner.sh" "$REPO/dev/lib/"
printf 'target/\nnode_modules/\n' >"$REPO/.gitignore"
g -C "$REPO" add -A
g -C "$REPO" commit -q -m fixture
g -C "$REPO" push -q -u origin main

# add_wt <name> <branch>: a linked worktree on a new pushed branch off main.
add_wt() {
    g -C "$REPO" worktree add -q -b "$2" "$WORK/$1" main
    g -C "$WORK/$1" push -q -u origin "$2" 2>/dev/null
}

add_wt merged feat/merged
echo merged >"$WORK/merged/merged.txt"
g -C "$WORK/merged" add merged.txt
g -C "$WORK/merged" commit -q -m merged
g -C "$WORK/merged" push -q
mkdir -p "$WORK/merged/target/debug"
g -C "$REPO" merge -q --ff-only feat/merged
g -C "$REPO" push -q

add_wt dirty feat/dirty
echo scratch >"$WORK/dirty/untracked.txt"

add_wt unpushed feat/unpushed
echo local >"$WORK/unpushed/local.txt"
g -C "$WORK/unpushed" add local.txt
g -C "$WORK/unpushed" commit -q -m local
mkdir -p "$WORK/unpushed/target/debug" "$WORK/unpushed/tools/node_modules/pkg"
echo build >"$WORK/unpushed/target/debug/out"

# Never pushed under this name, squash-merged as PR #42 from another branch.
g -C "$REPO" worktree add -q -b local/squashed "$WORK/squashed-wt" main
echo squashed >"$WORK/squashed-wt/squashed.txt"
g -C "$WORK/squashed-wt" add squashed.txt
g -C "$WORK/squashed-wt" commit -q -m squashed
git -C "$WORK/squashed-wt" rev-parse HEAD >"$WORK/squashed"

# Never pushed, but the same change reached main by another commit.
g -C "$REPO" worktree add -q -b local/picked "$WORK/picked" main
echo picked >"$WORK/picked/picked.txt"
g -C "$WORK/picked" add picked.txt
g -C "$WORK/picked" commit -q -m picked
cp "$WORK/picked/picked.txt" "$REPO/picked.txt"
g -C "$REPO" add picked.txt
g -C "$REPO" commit -q -m "picked (squashed)"
g -C "$REPO" push -q

g -C "$REPO" worktree add -q -b never-pushed "$WORK/never" main
echo mine >"$WORK/never/mine.txt"
g -C "$WORK/never" add mine.txt
g -C "$WORK/never" commit -q -m mine
g -C "$REPO" worktree add -q --detach "$WORK/detached" main
add_wt locked feat/locked
g -C "$REPO" worktree lock "$WORK/locked"
add_wt leased feat/leased
add_wt recent feat/recent

g -C "$REPO" fetch -q origin

# Everything three hours old; then the cases that must look busy.
find "$WORK" -exec perl -e 'my $t = time - 10800; utime $t, $t, @ARGV' {} +
printf 'owner=other\nclaimed_at=%s\ncaller=test\n' "$(date +%s)" \
    >"$(git -C "$WORK/leased" rev-parse --absolute-git-dir)/phux-worktree-owner"
touch "$WORK/recent/fresh.txt"

prune() { (cd "$REPO" && PATH="$SAFE_PATH" bash dev/worktree-prune.sh --idle-hours 1 "$@"); }

# ------------------------------------------------------------- dry run
OUT="$(prune)"
check "dry run plans the merged removal" '[[ "$OUT" == *"remove    $WORK/merged"* ]]'
check "dry run keeps the merged worktree" '[[ -d "$WORK/merged" ]]'
check "dry run keeps build output" '[[ -d "$WORK/unpushed/target" ]]'
check "dry run says how to apply" '[[ "$OUT" == *"Rerun with --apply"* ]]'

# ------------------------------------------------------------- outputs only
OUT="$(prune --apply --outputs-only)"
check "--outputs-only never removes a worktree" '[[ -d "$WORK/merged" ]]'
check "--outputs-only prunes an idle target" '[[ ! -e "$WORK/merged/target" ]]'
mkdir -p "$WORK/merged/target/debug" "$WORK/unpushed/target/debug"
find "$WORK/merged" "$WORK/unpushed" -exec perl -e 'my $t = time - 10800; utime $t, $t, @ARGV' {} +

# ------------------------------------------------------------- apply
OUT="$(prune --apply)"
check "removes the merged clean worktree" '[[ ! -e "$WORK/merged" ]]'
check "deletes its merged branch" '! git -C "$REPO" rev-parse -q --verify refs/heads/feat/merged >/dev/null'
check "keeps a dirty worktree" '[[ -f "$WORK/dirty/untracked.txt" ]]'
check "lists the dirty worktree" '[[ "$OUT" == *"$WORK/dirty (feat/dirty): uncommitted changes"* ]]'
check "keeps unpushed commits" '[[ -f "$WORK/unpushed/local.txt" ]]'
check "lists unpushed commits" '[[ "$OUT" == *"$WORK/unpushed (feat/unpushed): not landed, 1 unpushed commit(s)"* ]]'
check "removes work a merged PR holds" '[[ ! -e "$WORK/squashed-wt" && "$OUT" == *"landed (PR #42)"* ]]'
check "deletes the squashed branch" '! git -C "$REPO" rev-parse -q --verify refs/heads/local/squashed >/dev/null'
check "removes work already in main" '[[ ! -e "$WORK/picked" && "$OUT" == *"landed (already in origin/main)"* ]]'
check "prunes the idle target" '[[ ! -e "$WORK/unpushed/target" ]]'
check "prunes ignored node_modules" '[[ ! -e "$WORK/unpushed/tools/node_modules" ]]'
check "keeps never-pushed work nothing landed" '[[ -f "$WORK/never/mine.txt" && "$OUT" == *"$WORK/never (never-pushed): not landed"* ]]'
check "keeps a detached checkout" '[[ -d "$WORK/detached" && "$OUT" == *"detached checkout"* ]]'
check "skips a locked worktree" '[[ -d "$WORK/locked" && "$OUT" == *"$WORK/locked  (locked)"* ]]'
check "skips a live lease" '[[ -d "$WORK/leased" && "$OUT" == *"holds its lease"* ]]'
check "skips a recently touched worktree" '[[ -d "$WORK/recent" && "$OUT" == *"$WORK/recent  (touched"* ]]'
check "never touches the primary checkout" '[[ -f "$REPO/.gitignore" ]]'

if [[ "$FAILURES" -ne 0 ]]; then
    printf '%s failure(s)\n' "$FAILURES" >&2
    exit 1
fi
printf 'worktree-prune: ok\n'
