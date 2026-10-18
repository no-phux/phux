// Find releases that started and never finished: a draft past the grace
// window, a public release with no assets, a merged release PR Release Please
// never tagged, a manifest version with no release, and a stable channel
// manifest that does not name the newest published release. It reads GitHub
// state, not workflow logs, so it catches a stall whichever step failed.
import { readFileSync } from "node:fs";
import { describe, Failure } from "../lib/command.ts";
import { git } from "../lib/git.ts";
import { gh, ghLines } from "../lib/github.ts";
import { status } from "../lib/output.ts";
import { repoPath } from "../lib/repo.ts";
import { str, table, type Table } from "../lib/value.ts";

export interface Release {
  readonly tag: string;
  readonly draft: boolean;
  readonly prerelease: boolean;
  readonly assets: number;
  readonly created: string;
}

/** What the check reads; `stable` is the error text when channels/stable.json is unreadable. */
export interface Snapshot {
  readonly repo: string;
  readonly releases: readonly Release[];
  /** Merged PRs still labelled `autorelease: pending`: `{number, title, merged}`. */
  readonly pending: readonly Table[];
  /** .release-please-manifest.json, the config's `packages`, and when the manifest last moved. */
  readonly manifest: Table;
  readonly packages: Table;
  readonly bumped: string;
  readonly stable: Table | string;
}

/** The newest published `<prefix>X.Y.Z` version, or undefined. */
function newest(releases: readonly Release[], prefix: string): string | undefined {
  const pattern = new RegExp(`^${prefix}\\d+\\.\\d+\\.\\d+$`);
  return releases
    .filter((release) => !release.draft && !release.prerelease && pattern.test(release.tag))
    .map((release) => release.tag.slice(prefix.length))
    .sort((left, right) => left.localeCompare(right, undefined, { numeric: true }))
    .at(-1);
}

export function problems(state: Snapshot, graceMs: number, now = Date.now()): string[] {
  const { repo, releases } = state;
  const late = (time: string): boolean => now - Date.parse(time) > graceMs;
  const found: string[] = [];
  for (const release of releases) {
    if (release.draft && late(release.created)) {
      const minutes = Math.round((now - Date.parse(release.created)) / 60_000);
      found.push(
        `${release.tag} has been a draft for ${minutes} minutes. ` +
          `Finish it: gh workflow run release.yml --repo ${repo} -f tag=${release.tag}`,
      );
    }
    if (!release.draft && release.assets === 0) {
      found.push(`${release.tag} is published with no assets.`);
    }
  }
  // Release Please opens no next release PR while a merged one is still pending.
  for (const pr of state.pending.filter((pr) => late(str(pr["merged"]) ?? ""))) {
    found.push(
      `#${String(pr["number"])} ("${String(pr["title"])}") merged but is still "autorelease: pending"; Release Please cut no release for it.`,
    );
  }
  const tags = new Set(releases.map((release) => release.tag));
  for (const [path, version] of late(state.bumped) ? Object.entries(state.manifest) : []) {
    const component = str(table(state.packages[path])["component"]);
    const tag = `${component ? `${component}-` : ""}v${String(version)}`;
    if (!tags.has(tag)) found.push(`${path} is at ${String(version)}, but ${tag} has no release.`);
  }
  if (typeof state.stable === "string") {
    return [...found, `channels/stable.json is unreadable: ${state.stable}`];
  }
  for (const [product, prefix] of Object.entries({ phux: "v", desktop: "desktop-v" })) {
    const listed = str(table(table(state.stable["products"])[product])["version"]);
    const latest = newest(releases, prefix);
    if (listed !== latest) {
      found.push(
        `channels/stable.json names ${product} ${String(listed)}, but ${prefix}${String(latest)} is published. Rebuild it: gh workflow run release.yml --repo ${repo}`,
      );
    }
  }
  return found;
}

const readJson = (path: string): Table => table(JSON.parse(readFileSync(repoPath(path), "utf8")));

function stableManifest(repo: string): Table | string {
  try {
    const args = ["release", "download", "channels", "--repo", repo, "--pattern", "stable.json"];
    return table(JSON.parse(gh([...args, "--output", "-"])));
  } catch (error) {
    return describe(error);
  }
}

function snapshot(repo: string): Snapshot {
  const releases = ghLines([
    "api",
    `repos/${repo}/releases`,
    "--paginate",
    "--jq",
    ".[] | {tag: .tag_name, draft, prerelease, assets: (.assets | length), created: .created_at}",
  ]).map((value): Release => {
    const row = table(value);
    const [tag, created] = [str(row["tag"]) ?? "", str(row["created"]) ?? ""];
    const [draft, prerelease] = [row["draft"] === true, row["prerelease"] === true];
    return { tag, draft, prerelease, assets: Number(row["assets"]), created };
  });
  const pending = ghLines([
    "api",
    `repos/${repo}/issues?state=closed&labels=autorelease:%20pending&per_page=100`,
    "--jq",
    ".[] | select(.pull_request.merged_at != null) | {number, title, merged: .pull_request.merged_at}",
  ]).map(table);
  return {
    repo,
    releases,
    pending,
    manifest: readJson(".release-please-manifest.json"),
    packages: table(readJson("release-please-config.json")["packages"]),
    bumped: git(["log", "-1", "--format=%cI", "--", ".release-please-manifest.json"]).trim(),
    stable: stableManifest(repo),
  };
}

export function releaseDrift(graceMinutes: number): number {
  const repo = process.env["GITHUB_REPOSITORY"] ?? "no-phux/phux";
  // The API hides drafts from a token without push access, which would pass
  // every draft check vacuously. In Actions the job grants contents: write.
  if (process.env["GITHUB_ACTIONS"] !== "true") {
    const [access] = ghLines(["api", `repos/${repo}`, "--jq", "{push: .permissions.push}"]);
    if (table(access)["push"] !== true) {
      throw new Failure(`the token for ${repo} lacks push access, so draft releases are invisible`);
    }
  }
  const state = snapshot(repo);
  const found = problems(state, graceMinutes * 60_000);
  for (const problem of found) status("error", `stuck release: ${problem}`);
  if (found.length > 0) throw new Failure(`${found.length} stuck release problem(s)`);
  console.log(`release drift check passed: ${state.releases.length} releases`);
  return 0;
}
