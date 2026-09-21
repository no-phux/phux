# CI and release workflows

**TL;DR.** Twenty-four workflows in five lanes. Only `ci` and `commitlint`
gate `main`. Everything else either self-skips, deploys, or belongs to the
release train that `release-please.yml` owns end to end. The full runbook,
the known problems, and the daily health digest live in the private
`no-phux/ops` repository under `build-ops/`; this page is the in-repo map.

## The five lanes

| Lane | Workflows | Fires on |
|---|---|---|
| PR validation | `ci`, `cockpit-ci`, `conventional-commits`, `web-check`, `native-setup` | every non-draft pull request, and `main` |
| Opt-in checks | `stress` (label or dispatch), `mutation` (dispatch) | never automatically |
| Release train | `release-please` and the workflows it calls: `release`, `cockpit-release`, `ffi-xcframework`, `ffi-android`, `linear-release`; plus `agent-integration-release` on component tags, `next-release` off green `main`, `publish-crate` by hand | a push to `main`, then a tag. `ffi-android` also runs on `main` when the mobile shim changes, so the pin's SHA already has an artifact. |
| Site and worker | `site-deploy`, `site-deploy-worker`, `site-rollback-worker`, `site-native-control` | `main` pushes under `docs/`, or an operator |
| Scheduled ops | `release-drift` (daily), `site-native-monitor` (6-hourly), `native-setup` (weekly) | cron |

## What gates `main`

The ruleset requires two contexts: **`ci`** and **`commitlint`**.

`ci` is an aggregate job. It depends on every lane inside `ci.yml` and
applies the skip policy: the classify and workflow-gate jobs must succeed,
while the product lanes may succeed *or* skip. That is why `cockpit-ci`,
`web-check` and `native-setup` can self-skip without blocking a merge, and
why adding a new lane means adding it to that aggregate rather than to the
ruleset.

A ruleset matches a job's **display name**, not its id. Several jobs here
differ between the two (`changes` shows as "classify changes",
`cockpit-ci`'s `test` shows as "Zig 0.16 macOS"), so never assume an id is
requireable.

## Cancellation rules

Pull requests are latest-wins: a new push cancels the in-flight run.

Pushes to `main` are deliberately **not** latest-wins in `ci` and
`cockpit-ci`. Their concurrency group includes the commit SHA, so every
`main` commit keeps its own validation. A release can be cut from any of
them, and `release-please` refuses to build a tag until it finds a
successful run at that exact SHA.

Three workflows do not follow that rule and cancel each other on
consecutive `main` pushes: `web-check`, `native-setup` and `site-deploy`.
For the site deploy that is correct. For the other two it means a fast
sequence of merges can leave a commit unvalidated.

Publishing lanes set `cancel-in-progress: false` so an upload is never
interrupted, with one exception: `next-release` coalesces by design.
`publish-crate` declares no concurrency at all; its reviewer-gated
environment is what serializes it.

The three worker workflows share one group on purpose, so a deploy, a
rollback and the kill switch can never mutate the Worker at the same time.
Renaming any one of those groups breaks that guarantee.

## When something goes wrong

- **Red `main`.** Check `ci` first, then `just e2e` locally: the end-to-end
  journey is not inside `just ci`, so a green `ci` can still ship a break.
- **A release stalled.** `release-drift` alarms daily on stuck drafts and
  manifest-versus-tag gaps. Recovery is a manual dispatch of the workflow
  that failed, with the tag as input; see the ops runbook.
- **A closed PR still burning runners.** `pr-janitor` cancels runs and
  deletes caches for a closed PR, matching by head SHA so `main` is never
  touched.
- **Cockpit release stopped with `KEYLESS_RELEASE_STOP`.** The signing or
  tap secrets are missing; assets stay on the draft until they are set.

## Conventions

Action versions are SHA-pinned and checked by `just workflow-check`, which
also validates the path-routing table and the syntax of every file here.
Shared logic belongs in `.github/actions/` (`classify-changes`,
`validation-receipt`, `setup-rust-lane`, `cockpit-rust-artifacts`) rather
than copied between workflows.
