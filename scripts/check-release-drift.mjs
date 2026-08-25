#!/usr/bin/env node

// Detect releases that were started and never finished.
//
// Every release defect this repo has hit failed SILENTLY. v0.19.0's release
// step aborted inside a green release-please run and left no tag. The agent
// integration lane failed on all four of its first invocations and left four
// permanent 0-asset drafts. Nothing was red, so nothing was noticed — a release
// that never happens looks exactly like a release nobody cut.
//
// This is the sentinel for that whole class. It asserts the three end states a
// finished release must have, and is deliberately independent of the workflows
// it polices: it reads GitHub, not the run logs, so it catches a stuck release
// no matter which step failed or whether that step even ran.
//
// Run by release-drift.yml on a schedule, and available locally as
// `just release-drift`. Requires an authenticated `gh`.
//
// GRACE_MINUTES exists because a release is legitimately mid-flight for a few
// minutes: the tag lands, the artifact build takes ~10, the draft is public
// only at the end. Anything still unfinished after the grace window is stuck,
// not busy.

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const repo = process.env.GITHUB_REPOSITORY ?? "phall1/phux";
const graceMinutes = Number(process.env.GRACE_MINUTES ?? 120);
assert.ok(Number.isFinite(graceMinutes) && graceMinutes >= 0, "GRACE_MINUTES must be a non-negative number");

const now = Date.now();
const graceMs = graceMinutes * 60_000;
const failures = [];

function grantsContentsWrite(workflow) {
  let inTopPermissions = false;
  let inDrift = false;
  let inDriftPermissions = false;
  let topWrite = false;
  let driftOverride = false;
  let driftWrite = false;

  for (const line of workflow.split("\n")) {
    if (/^permissions:\s*$/.test(line)) {
      inTopPermissions = true;
      continue;
    }
    if (inTopPermissions && /^[^\s#]/.test(line)) inTopPermissions = false;
    if (inTopPermissions && /^  contents:\s*write(?:\s*#.*)?$/.test(line)) topWrite = true;

    if (/^  drift:\s*$/.test(line)) {
      inDrift = true;
      continue;
    }
    if (inDrift && /^  [^\s#]/.test(line)) {
      inDrift = false;
      inDriftPermissions = false;
    }
    if (inDrift && /^    permissions:\s*$/.test(line)) {
      driftOverride = true;
      inDriftPermissions = true;
      continue;
    }
    if (inDriftPermissions && /^    [^\s#]/.test(line)) inDriftPermissions = false;
    if (inDriftPermissions && /^      contents:\s*write(?:\s*#.*)?$/.test(line)) driftWrite = true;
  }

  return topWrite && (!driftOverride || driftWrite);
}

// A token without push access is not shown draft releases at all — the REST
// API simply omits them. Two of the four checks below are about drafts, so
// under such a token they cannot fail: an empty draft list is indistinguishable
// from a healthy one, and this check would report success on a repository full
// of stuck releases.
//
// That is the precise failure mode this whole script exists to catch, so it
// refuses to run blind rather than passing quietly. A GITHUB_TOKEN is a GitHub
// App installation token, though, and the repository API's user-oriented
// `.permissions.push` field is false even when the workflow grants
// `contents: write`. In Actions, validate the checked-out workflow declaration;
// outside Actions, retain the live user-token check.
if (process.env.GITHUB_ACTIONS === "true") {
  const workflowPath = process.env.PHUX_DRIFT_WORKFLOW ?? join(root, ".github/workflows/release-drift.yml");
  const workflow = await readFile(workflowPath, "utf8");
  assert.ok(
    grantsContentsWrite(workflow),
    "release-drift.yml does not grant effective contents: write to jobs.drift, " +
      "so draft releases may be hidden.",
  );
} else {
  const canSeeDrafts = gh(["api", `repos/${repo}`, "--jq", "{push: .permissions.push}"])[0]?.push;
  assert.ok(
    canSeeDrafts,
    `the token for ${repo} lacks push access, so the GitHub API will not list draft releases. ` +
      "Two of this check's four assertions are about drafts and would pass vacuously. " +
      "Grant `contents: write` (read-only for this job's purposes — see release-drift.yml).",
  );
}

const releases = gh([
  "api",
  `repos/${repo}/releases`,
  "--paginate",
  "--jq",
  ".[] | {tag: .tag_name, draft: .draft, assets: (.assets | length), created: .created_at}",
]);

// 1. A draft that outlived the grace window is a release whose publish step
//    never completed. This is the agent-integration-release.yml failure mode:
//    tag created, draft created, artifacts never attached, draft never flipped.
for (const release of releases) {
  if (!release.draft) continue;
  const ageMs = now - Date.parse(release.created);
  if (ageMs > graceMs) {
    failures.push(
      `${release.tag} has been a draft for ${minutes(ageMs)} minutes (grace ${graceMinutes}). ` +
        `Its publish lane never finished. Re-run it: ` +
        `gh workflow run ${workflowFor(release.tag)} --repo ${repo} -f tag=${release.tag}${
          release.tag.startsWith("v") ? "" : " -f dry_run=false"
        }`,
    );
  }
}

// 2. A published release with no assets is a release users cannot install. Every
//    release in this repo's history carries its binaries or its packed tarball,
//    so zero is never legitimate — it means the publish step flipped the draft
//    before, or instead of, uploading.
for (const release of releases) {
  if (release.draft) continue;
  if (release.assets === 0) {
    failures.push(`${release.tag} is published with zero assets — nothing to download.`);
  }
}

// 3. A merged release PR still labelled `autorelease: pending` means
//    release-please built no release for it. This is the exact v0.19.0 defect,
//    and it is self-perpetuating: release-please refuses to open the NEXT
//    release PR while an untagged merged one is outstanding, so the whole
//    pipeline stops until a human notices.
const pendingPrs = gh([
  "api",
  `repos/${repo}/issues?state=closed&labels=autorelease:%20pending&per_page=100`,
  "--jq",
  ".[] | select(.pull_request != null) | {number: .number, title: .title, merged: .pull_request.merged_at}",
]);
for (const pr of pendingPrs) {
  if (!pr.merged) continue;
  const ageMs = now - Date.parse(pr.merged);
  if (ageMs > graceMs) {
    failures.push(
      `#${pr.number} ("${pr.title}") merged ${minutes(ageMs)} minutes ago and is still labelled ` +
        `"autorelease: pending" — release-please created no release for it, and will refuse to open ` +
        `the next release PR until this is resolved.`,
    );
  }
}

// 4. Every version the manifest claims to have released must have its tag. The
//    manifest is bumped by the release PR itself, so a bumped version with no
//    tag is a release that was prepared and then dropped on the floor.
const manifest = JSON.parse(await readFile(join(root, ".release-please-manifest.json"), "utf8"));
const config = JSON.parse(await readFile(join(root, "release-please-config.json"), "utf8"));
const manifestTouched = Date.parse(
  execFileSync("git", ["log", "-1", "--format=%cI", "--", ".release-please-manifest.json"], {
    cwd: root,
    encoding: "utf8",
  }).trim(),
);
if (Number.isFinite(manifestTouched) && now - manifestTouched > graceMs) {
  const tags = new Set(releases.map((release) => release.tag));
  for (const [path, version] of Object.entries(manifest)) {
    const component = config.packages?.[path]?.component;
    const tag = component ? `${component}-v${version}` : `v${version}`;
    if (!tags.has(tag)) {
      failures.push(`${path} is at ${version} in the manifest, but ${tag} has no GitHub release.`);
    }
  }
}

if (failures.length > 0) {
  for (const failure of failures) process.stderr.write(`stuck release: ${failure}\n`);
  process.stderr.write(`release drift check failed: ${failures.length} stuck release(s)\n`);
  process.exit(1);
}

process.stdout.write(
  `release drift check passed: ${releases.length} releases, no stuck drafts, no assetless publishes, no pending release PRs\n`,
);

function gh(args) {
  const out = execFileSync("gh", args, { encoding: "utf8", maxBuffer: 32 * 1024 * 1024 });
  return out
    .split("\n")
    .filter((line) => line.trim() !== "")
    .map((line) => JSON.parse(line));
}

function minutes(ms) {
  return Math.round(ms / 60_000);
}

function workflowFor(tag) {
  return tag.startsWith("v") ? "release.yml" : "agent-integration-release.yml";
}
