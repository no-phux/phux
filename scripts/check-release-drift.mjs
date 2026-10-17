#!/usr/bin/env node

// Find releases that started and never finished: a draft past the grace
// window, a public release with no assets, a merged release PR Release Please
// never tagged, a manifest version with no release, and a stable channel
// manifest that does not name the newest published release. It reads GitHub
// state, not workflow logs, so it catches a stall whichever step failed.
// publish.yml runs it daily; `just release-drift` runs it locally.

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const repo = process.env.GITHUB_REPOSITORY ?? "no-phux/phux";
const graceMs = Number(process.env.GRACE_MINUTES ?? 120) * 60_000;
const now = Date.now();
const failures = [];

const gh = (...args) =>
  execFileSync("gh", args, { encoding: "utf8", maxBuffer: 64 * 1024 * 1024 })
    .split("\n")
    .filter((line) => line.trim())
    .map((line) => JSON.parse(line));
const minutes = (ms) => Math.round(ms / 60_000);

// The API hides drafts from a token without push access, which would pass
// every draft check vacuously. In Actions the job grants contents: write.
if (process.env.GITHUB_ACTIONS !== "true" && !gh("api", `repos/${repo}`, "--jq", "{push: .permissions.push}")[0].push) {
  throw new Error(`the token for ${repo} lacks push access, so draft releases are invisible`);
}

const releases = gh(
  "api", `repos/${repo}/releases`, "--paginate", "--jq",
  ".[] | {tag: .tag_name, draft, prerelease, assets: (.assets | length), created: .created_at}",
);

for (const release of releases) {
  const age = now - Date.parse(release.created);
  if (release.draft && age > graceMs) {
    failures.push(
      `${release.tag} has been a draft for ${minutes(age)} minutes. ` +
        `Finish it: gh workflow run publish.yml --repo ${repo} -f tag=${release.tag}`,
    );
  }
  if (!release.draft && release.assets === 0) {
    failures.push(`${release.tag} is published with no assets.`);
  }
}

// Release Please refuses to open the next release PR while a merged one is
// still pending, so this one stalls the whole pipeline.
const pending = gh(
  "api", `repos/${repo}/issues?state=closed&labels=autorelease:%20pending&per_page=100`, "--jq",
  ".[] | select(.pull_request.merged_at != null) | {number, title, merged: .pull_request.merged_at}",
);
for (const pr of pending) {
  if (now - Date.parse(pr.merged) > graceMs) {
    failures.push(`#${pr.number} ("${pr.title}") merged but is still "autorelease: pending"; Release Please cut no release for it.`);
  }
}

const tags = new Set(releases.map((release) => release.tag));
const manifest = JSON.parse(readFileSync(join(root, ".release-please-manifest.json"), "utf8"));
const config = JSON.parse(readFileSync(join(root, "release-please-config.json"), "utf8"));
const bumped = Date.parse(
  execFileSync("git", ["log", "-1", "--format=%cI", "--", ".release-please-manifest.json"], { cwd: root, encoding: "utf8" }).trim(),
);
if (now - bumped > graceMs) {
  for (const [path, version] of Object.entries(manifest)) {
    const component = config.packages[path]?.component;
    const tag = component ? `${component}-v${version}` : `v${version}`;
    if (!tags.has(tag)) failures.push(`${path} is at ${version}, but ${tag} has no release.`);
  }
}

const newest = (prefix) =>
  releases
    .filter((r) => !r.draft && !r.prerelease && new RegExp(`^${prefix}\\d+\\.\\d+\\.\\d+$`).test(r.tag))
    .map((r) => r.tag.slice(prefix.length))
    .sort((a, b) => a.localeCompare(b, undefined, { numeric: true }))
    .at(-1);
let stable = null;
try {
  stable = JSON.parse(execFileSync(
    "gh", ["release", "download", "channels", "--repo", repo, "--pattern", "stable.json", "--output", "-"],
    { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] },
  ));
} catch (error) {
  failures.push(`channels/stable.json is unreadable: ${error.stderr?.trim() || error.message}`);
}
for (const [product, prefix] of stable ? [["phux", "v"], ["desktop", "desktop-v"]] : []) {
  const listed = stable.products[product]?.version;
  if (listed !== newest(prefix)) {
    failures.push(`channels/stable.json names ${product} ${listed}, but ${prefix}${newest(prefix)} is published. Rebuild it: gh workflow run publish.yml --repo ${repo}`);
  }
}

if (failures.length > 0) {
  for (const failure of failures) process.stderr.write(`stuck release: ${failure}\n`);
  process.exit(1);
}
process.stdout.write(`release drift check passed: ${releases.length} releases\n`);
