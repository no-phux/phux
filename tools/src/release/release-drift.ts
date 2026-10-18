// Find releases that started and never finished: a draft past the grace
// window, a public release with no assets, a merged release PR Release Please
// never tagged, a manifest version with no release, and a stable channel
// manifest that does not name the newest published release. It reads GitHub
// state, not workflow logs, so it catches a stall whichever step failed.
// publish.yml runs it daily; `just release-drift` runs it locally.
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { git } from "../lib/git.ts";
import { gh, GhError, ghLines } from "../lib/github.ts";
import { ROOT } from "../lib/repo.ts";
import { str, table, type Table } from "../lib/value.ts";

interface Release {
  readonly tag: string;
  readonly draft: boolean;
  readonly prerelease: boolean;
  readonly assets: number;
  readonly created: string;
}

const minutes = (ms: number): number => Math.round(ms / 60_000);

function releaseRow(value: unknown): Release {
  const row = table(value);
  return {
    tag: str(row["tag"]) ?? "",
    draft: row["draft"] === true,
    prerelease: row["prerelease"] === true,
    assets: Number(row["assets"]),
    created: str(row["created"]) ?? "",
  };
}

function readJson(path: string): Table {
  return table(JSON.parse(readFileSync(join(ROOT, path), "utf8")));
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

function stableManifest(repo: string, failures: string[]): Table | undefined {
  try {
    const args = ["release", "download", "channels", "--repo", repo];
    return table(JSON.parse(gh([...args, "--pattern", "stable.json", "--output", "-"])));
  } catch (error) {
    const reason =
      (error instanceof GhError && error.stderr.trim()) ||
      (error instanceof Error ? error.message : String(error));
    failures.push(`channels/stable.json is unreadable: ${reason}`);
    return undefined;
  }
}

export function releaseDrift(): number {
  const env = process.env;
  const repo = env["GITHUB_REPOSITORY"] ?? "no-phux/phux";
  const graceMs = Number(env["GRACE_MINUTES"] ?? 120) * 60_000;
  const now = Date.now();
  const failures: string[] = [];

  // The API hides drafts from a token without push access, which would pass
  // every draft check vacuously. In Actions the job grants contents: write.
  if (env["GITHUB_ACTIONS"] !== "true") {
    const [access] = ghLines(["api", `repos/${repo}`, "--jq", "{push: .permissions.push}"]);
    if (table(access)["push"] !== true) {
      console.error(
        `error: the token for ${repo} lacks push access, so draft releases are invisible`,
      );
      return 1;
    }
  }

  const releases = ghLines([
    "api",
    `repos/${repo}/releases`,
    "--paginate",
    "--jq",
    ".[] | {tag: .tag_name, draft, prerelease, assets: (.assets | length), created: .created_at}",
  ]).map(releaseRow);

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
  const pending = ghLines([
    "api",
    `repos/${repo}/issues?state=closed&labels=autorelease:%20pending&per_page=100`,
    "--jq",
    ".[] | select(.pull_request.merged_at != null) | {number, title, merged: .pull_request.merged_at}",
  ]).map(table);
  for (const pr of pending) {
    if (now - Date.parse(str(pr["merged"]) ?? "") > graceMs) {
      failures.push(
        `#${String(pr["number"])} ("${String(pr["title"])}") merged but is still "autorelease: pending"; Release Please cut no release for it.`,
      );
    }
  }

  const tags = new Set(releases.map((release) => release.tag));
  const manifest = readJson(".release-please-manifest.json");
  const packages = table(readJson("release-please-config.json")["packages"]);
  const bumped = Date.parse(
    git(["log", "-1", "--format=%cI", "--", ".release-please-manifest.json"]).trim(),
  );
  if (now - bumped > graceMs) {
    for (const [path, version] of Object.entries(manifest)) {
      const component = str(table(packages[path])["component"]);
      const tag = component ? `${component}-v${String(version)}` : `v${String(version)}`;
      if (!tags.has(tag))
        failures.push(`${path} is at ${String(version)}, but ${tag} has no release.`);
    }
  }

  const stable = stableManifest(repo, failures);
  if (stable !== undefined) {
    for (const [product, prefix] of [
      ["phux", "v"],
      ["desktop", "desktop-v"],
    ] as const) {
      const listed = str(table(table(stable["products"])[product])["version"]);
      const latest = newest(releases, prefix);
      if (listed !== latest) {
        failures.push(
          `channels/stable.json names ${product} ${String(listed)}, but ${prefix}${String(latest)} is published. Rebuild it: gh workflow run publish.yml --repo ${repo}`,
        );
      }
    }
  }

  if (failures.length > 0) {
    for (const failure of failures) process.stderr.write(`stuck release: ${failure}\n`);
    return 1;
  }
  process.stdout.write(`release drift check passed: ${releases.length} releases\n`);
  return 0;
}
