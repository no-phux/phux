---
audience: agents
stability: scratch
last-reviewed: 2026-10-07
---
# Finishing Mimo's server fixes

The operator asked to finish Mimo's phux bugs. The primary checkout contains
three unfinished files on fix/agent-session-unseal-ordering, plus the committed
actor-ordering fix in PR #1056. Its current main is newer. Created isolated
worktree phux-worktrees/mimo-server-bugs from origin/main 6f82321f, cherry-picked
only the actor fix, and applied a copy of the unfinished diff. The primary
checkout's source remains intact. No production server, socket, state or binary
is used for validation.

The upgrade invariant is that an awaited preparation failure resumes every
carried agent stream before returning the error. Keep Drop as cancellation and
exec-failure fallback, but verify full mailboxes, lost actors, cancelled lift,
sequence continuity and actual preparation failures. Check that acknowledging
unseal cannot hang the server indefinitely when a cut did not answer.

The terminal invariant is that a fenced consumer can publish one contiguous
replacement bootstrap when mailbox capacity becomes available, with no partial
replacement on timeout and final screen before close. The unfinished change
adds a bounded wait to gap resync but repeats the old branch; its correctness
must be checked against the existing exit ordering and retry budget. Use paused
time and explicitly polled reservations to test drain, timeout, cancellation and
close without relying on scheduling sleeps. Retain the real PTY convergence
regression and diagnostics.

Independent source reviews will examine each invariant before implementation,
then review the final diff after targeted red-to-green tests. Run the smallest
server gates first, broaden to the server suite and relevant upgrade/resync
integration lanes, and report the actual scope rather than claim full CI.
Beads owns task status; this entry records the design and evidence.

The source reviews found an unbounded serial unseal (which could strand healthy
neighbors), gap waiting that lets close overtake the final bootstrap, and an
oversized sequential branch that cannot safely return Deferred after a partial
publication. Chose concurrent bounded rollback in the upgrade lane. For natural
terminal close, use tracked per-client drain fences and a fresh final actor
capture after EOF/purge, while actor cancellation remains deferred. This avoids
both lost Exit broadcasts and remembered-Exit ambiguity across last-shell
replacement or retained expiry. Wait for each client's final publication only
in its own close future; committed kills keep their existing abort semantics.
Independent source review accepted this refinement subject to concrete tests,
bounded snapshot allocation, and proper invalidated-native-capture handling.

Pinned Rust 1.99 and Zig 0.16 native doctor passed. Installed the repository's
pinned mbx 1.22 with the upstream archive SHA256 check; targets remain isolated
and the shared compiler cache avoids duplicate dependency compilation. The
legacy Beads export could not be imported by the installed newer CLI because
comment IDs changed type. Original export is intact. Local task phux-pfd tracks
this work. The CLI unexpectedly auto-committed local backups and attempted git push;
both attempts were rejected (the primary branch was behind, and the isolated
branch has no matching remote upstream). Disabled both automatic backup and git
push with BD_BACKUP_ENABLED=false and BD_BACKUP_GIT_PUSH=false; backup status
confirmed both effective settings. Removed the task-owned backup commits from
the isolated feature branch while preserving source. The relevant environment
keys come from the pinned CLI's [configuration implementation](https://github.com/gastownhall/beads/blob/v0.58.0/internal/config/config.go).
The primary source remains intact.


Red evidence: the inherited exit-resync unit test passed, but the new real-actor
natural-close test failed with “close overtook the final screen.” The latter
starts with no Exit broadcast, so it covers missed/consumed exit notifications
rather than assuming a reservation was already queued. The three fitting-batch
queue tests check whole-batch reservation, timeout without prefix publication,
and cancellation returning capacity. Final close takes fresh bounded actor
captures and awaits per-client completion fences. Retained task ownership and a
short-lived retired-wire lookup allow DETACH_RESOURCE to stop pending final
publication even after resource reap. Completion guards resolve on abort before
the first poll. Final native capture uses a request-only timeout; invalidation
is a failure, and dropped actors preserve PaneGone handling.

A preexisting overlapping-upgrade seal ownership race is separately tracked as
phux-nnz: seals are boolean, so one connection's failed preparation can unseal
another connection's cut. The scoped work fixes rollback of one preparation;
it does not claim concurrent upgrade preparation is serialized.

The tracker initialization's task-owned primary backup commit and configuration
were undone without resetting, stashing, cleaning or modifying primary source.
A compare-and-swap ref update removed only that automatic backup commit; only
its newly tracked backup paths were removed from the index. Configuration files
were restored to their pre-initialization contents. The three original Mimo
source-file diffs were checked byte-for-byte against the saved initial patch.
The primary checkout is again at its original source commit and original dirty
source state. Local tracker data and backup records remain available.


Final validation on the combined isolated branch:

- `mbx +1.99.0 nextest run --locked -p phux-server --lib --test terminal --test attach --test lifecycle --test-threads=4`: 1,358 passed, zero skipped. This includes both real PTY lag/convergence regressions, retained expiry/purge, last-shell EOF, native and synthesized detach, the actor-ordering and upgrade failure tests, and all six new final-close cases. The close regression was observed red before implementation.
- `mbx +1.99.0 clippy --locked -p phux-server --all-targets -- -D warnings`: passed (final execution exited zero). Production and test targets compiled with final guard-field names.
- `RUSTDOCFLAGS='-D warnings' mbx +1.99.0 doc --locked -p phux-server --no-deps`: passed.
- `mbx +1.99.0 check --locked -p phux-server --no-default-features --lib`: passed; the twelve feature-disabled warnings concern preexisting imports/mutability/native-only fields.
- `cargo fmt --all -- --check`, `git diff --check`, cache-portability guard: passed. Documentation gate checked 248 files with zero violations.

Independent production-source and oracle reviews found no remaining blocking
finding. Actual lifecycle tests substantiate EOF/retention behavior; the new
manually-polled fixtures substantiate full-mailbox ordering and cancellation.
This was scoped server validation, not `just ci-full` or a release preflight.

At the operator's request, updated and undrafted [PR #1056](https://github.com/no-phux/phux/pull/1056).
Its branch was advanced without force push from its existing head, merged with
current main, and given the verified rollback commit. Final PR head b399797a
has the same Rust sources as the reviewed upgrade worktree; the only newer
upstream differences are four CI/release-documentation files. Rewrote its title
and description around both queued-unseal ordering and bounded awaited rollback,
with actual validation and the concurrent-prepare limitation. Terminal resync is
kept as a separate commit for separate review.

Bead phux-pfd is closed; phux-nnz remains open for seal ownership. Exported only
these task-owned records and appended them to the passive historical export,
preserving all 1,870 earlier records. Local tracker operations have automatic
backup and remote synchronization disabled. The primary source and installed
phux remain untouched.

Prepared the independent resync-only branch `fix/mimo-terminal-resync` from
origin/main 0c7946e0, with source commit 195e4b8d. Its production and test targets
also passed `mbx +1.99.0 check --locked -p phux-server --all-targets` without the
upgrade PR's changes. The 1,358-test execution remains the combined-fixes run
recorded above. This separate branch is committed and clean; it has not been
pushed or made into a remote PR.


At the operator's request, published the independent terminal-resync branch and
opened [PR #1069](https://github.com/no-phux/phux/pull/1069) for review against
`no-phux/phux:main`. The fork head is
`antimemeai/phux:fix/mimo-terminal-resync`; its merge base is upstream main
0c7946e000154ba096050d66cb1e993ce8012a9d. The diff excludes the upgrade and
agent-session ordering changes from #1056. The PR description distinguishes
the standalone all-target compilation from the combined 1,358-test execution.
Local main was fast-forwarded to the same upstream commit with a guarded ref
update; it is not checked out in any worktree. The dirty primary source and
installed phux remain untouched. This journal update changes no Rust sources.
