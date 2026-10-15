# CI and release workflows

**TL;DR.** Workflows run in five lanes. Only `ci` and `commitlint`
gate `main`. Everything else either self-skips, deploys, or belongs to the
release train that `release-please.yml` owns end to end. The full runbook,
the known problems, and the daily health digest live in the private
`no-phux/ops` repository under `build-ops/`; this page is the in-repo map.

## The five lanes

| Lane | Workflows | Fires on |
|---|---|---|
| PR validation | `ci`, `conventional-commits`, `web-check`, `native-setup` | every non-draft pull request, and `main` |
| Opt-in checks | `stress` (label or dispatch), `mutation` (dispatch) | never automatically |
| Release train | `release-please`, `publish` and its leaf workflows: `release`, `desktop-release`, `ffi-xcframework`, `ffi-android`, `linear-release`; plus `agent-integration-release` on component tags, `next-release` off green `main`, `publish-crate` by hand | a push to `main`, then a tag. `ffi-android` also runs on `main` when the mobile shim changes, so the pin's SHA already has an artifact. |
| Site and worker | `site-deploy`, `site-deploy-worker`, `site-rollback-worker`, `site-native-control` | `main` pushes under `docs/`, or an operator |
| Scheduled ops | `release-drift` (daily), `site-native-monitor` (6-hourly), `native-setup` (weekly), `perf-nightly` (nightly; artifacts only, not a required check) | cron |

## What gates `main`

The ruleset requires two contexts: **`ci`** and **`commitlint`**.

`ci` is an aggregate job. It depends on every lane inside `ci.yml` and
applies the skip policy: the classify and workflow-gate jobs must succeed,
while the product lanes may succeed *or* skip. That is why
`web-check` and `native-setup` can self-skip without blocking a merge, and
why adding a new lane means adding it to that aggregate rather than to the
ruleset. `perf-icount` is one of those lanes: the aggregate fails when it
fails, and a path-filtered skip is still green. `perf-nightly` is not in
the aggregate.

Browser wasm coverage lives in `web-check.yml`. On browser changes that
workflow runs `wasm-pack test --node` for `clients/phux-web` and
`clients/phux-vt-web`, `wasm-pack build --target web --release` for the
shipping phux-web package, and `scripts/build-vt-wasm.sh --check` when the
vendored engine changes. The lane is a Nix develop plus wasm-pack plus
headless Chrome job with a 30-minute budget, so it stays off the required
`ci` aggregate. A wasm break fails the `web-check` run and can still reach
`main`, because merge protection is `ci` plus `commitlint`. That gap is the
accepted rot risk: the check is real, and it is too expensive to duplicate
inside `ci.yml`.

A ruleset matches a job's **display name**, not its id. Several jobs here
differ between the two (`changes` shows as "classify changes"), so never
assume an id is requireable.

## Cancellation rules

Pull requests are latest-wins: a new push cancels the in-flight run.

Pushes to `main` are deliberately **not** latest-wins in `ci`.
Its concurrency group includes the commit SHA, so every
`main` commit keeps its own validation. A release can be cut from any of
them, and `release-please` refuses to build a tag until it finds a
successful run at that exact SHA.

`web-check` and `native-setup` use the same SHA-keyed pattern as `ci` on
`main`, so consecutive merges do not cancel each other's validation.
`site-deploy` uses a single global group with `cancel-in-progress: false`
(matching the Worker deploy lane) so a mid-wrangler deploy is never killed;
new pushes queue instead.

Publishing lanes set `cancel-in-progress: false` so an upload is never
interrupted. `publish-crate` also declares an explicit per-tag concurrency
group; the `crates-io` environment reviewer is not enough on its own.
`site-deploy` still lists `release: [published]`, but GitHub suppresses
events raised by `GITHUB_TOKEN`, so badge refresh after a release currently
depends on the Monday cron or a manual dispatch (see private
`no-phux/ops` `build-ops/known-problems.md` §5).

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

## Conventions

Action versions are SHA-pinned and checked by `just workflow-check`, which
also validates the path-routing table and the syntax of every file here.
Shared logic belongs in `.github/actions/` (`classify-changes`,
`validation-receipt`, `setup-rust-lane`) rather
than copied between workflows.
