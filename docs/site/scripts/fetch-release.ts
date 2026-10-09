#!/usr/bin/env bun
/**
 * fetch-release.ts — stamp the site with the latest phux releases.
 *
 * Fetches the newest GitHub releases for no-phux/phux and writes
 * src/lib/release.json, which index.astro imports at build time (the version
 * badges next to the install commands). Runs as part of `bun run build`, so the
 * badges are always current with the deployed build.
 *
 * Freshness is driven by CI: site-deploy.yml fires on release:published and
 * rebuilds, so a new release ships the badges within minutes. The weekly cron
 * in that workflow is the backstop.
 *
 * Failure policy: the badges are decorative — if GitHub is unreachable or rate
 * limits the build, keep any previously generated release.json, else write
 * { "tag": null } and let the page omit the badges. Never fail the build.
 */

import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";

const ROOT = join(import.meta.dir, "..");
const OUT = join(ROOT, "src/lib/release.json");
const API = "https://api.github.com/repos/no-phux/phux/releases?per_page=30";

interface Release {
  tag: string | null;
  url: string | null;
  publishedAt: string | null;
}

interface GitHubRelease {
  tag_name?: string;
  html_url?: string;
  published_at?: string;
  draft?: boolean;
  prerelease?: boolean;
}

function latestRelease(
  list: GitHubRelease[],
  tagPattern: RegExp,
  allowPrereleases = false,
): Release | null {
  let latest: GitHubRelease | null = null;
  let latestVersion: bigint[] | null = null;
  for (const release of list) {
    if (release.draft || (!allowPrereleases && release.prerelease)) continue;
    const match = tagPattern.exec(release.tag_name ?? "");
    if (!match) continue;
    const version = match.slice(1).map((part) => BigInt(part));
    // GitHub's release order is not semantic version order. Compare each
    // numeric component, including the desktop alpha number, in precedence order.
    let newer = latestVersion === null;
    if (latestVersion) {
      for (let i = 0; i < version.length; i++) {
        if (version[i] === latestVersion[i]) continue;
        newer = version[i]! > latestVersion[i]!;
        break;
      }
    }
    if (newer) {
      latest = release;
      latestVersion = version;
    }
  }
  if (!latest?.tag_name) return null;
  return {
    tag: latest.tag_name,
    url: latest.html_url ?? null,
    publishedAt: latest.published_at ?? null,
  };
}

export function latestCoreRelease(list: GitHubRelease[]): Release | null {
  // Only bare vX.Y.Z tags represent stable core phux CLI releases.
  return latestRelease(list, /^v(\d+)\.(\d+)\.(\d+)$/);
}

export function latestCockpitRelease(list: GitHubRelease[]): Release | null {
  return latestRelease(list, /^cockpit-v(\d+)\.(\d+)\.(\d+)$/);
}

export function latestDesktopRelease(list: GitHubRelease[]): Release | null {
  // Desktop alphas are intentionally published as prereleases.
  return latestRelease(list, /^desktop-v(\d+)\.(\d+)\.(\d+)-alpha\.([1-9]\d*)$/, true);
}

interface StampedReleases {
  tag: string | null;
  url: string | null;
  publishedAt: string | null;
  cockpit: Release;
  desktop: Release;
}

const EMPTY: Release = { tag: null, url: null, publishedAt: null };

async function main() {
  try {
    const githubToken = process.env.GITHUB_TOKEN;
    const list: GitHubRelease[] = [];
    // Match the installer's bounded recent window; any later page can contain
    // a higher version even when all three streams already have a candidate.
    for (let page = 1; page <= 10; page++) {
      const res = await fetch(`${API}&page=${page}`, {
        headers: {
          Accept: "application/vnd.github+json",
          "User-Agent": "phux-site-build",
          ...(githubToken ? { Authorization: `Bearer ${githubToken}` } : {}),
        },
        signal: AbortSignal.timeout(10_000),
      });
      if (!res.ok) throw new Error(`github api ${res.status}`);
      const releases = (await res.json()) as GitHubRelease[];
      list.push(...releases);
      if (releases.length < 30) break;
    }
    const release = latestCoreRelease(list);
    if (!release) throw new Error("no core phux release found in recent releases");
    const stamped: StampedReleases = {
      ...release,
      cockpit: latestCockpitRelease(list) ?? { ...EMPTY },
      desktop: latestDesktopRelease(list) ?? { ...EMPTY },
    };
    await writeFile(OUT, JSON.stringify(stamped, null, 2) + "\n");
    console.log(`fetch-release: stamped ${release.tag} + ${stamped.cockpit.tag ?? "no-cockpit"} + ${stamped.desktop.tag ?? "no-desktop"}`);
  } catch (err) {
    // Keep a previously stamped file if one exists; otherwise omit the badges.
    try {
      await readFile(OUT, "utf8");
      console.warn(`fetch-release: fetch failed (${err}); keeping existing release.json`);
    } catch {
      const empty: StampedReleases = { ...EMPTY, cockpit: { ...EMPTY }, desktop: { ...EMPTY } };
      await writeFile(OUT, JSON.stringify(empty, null, 2) + "\n");
      console.warn(`fetch-release: fetch failed (${err}); badges omitted this build`);
    }
  }
}

if (import.meta.main) await main();
