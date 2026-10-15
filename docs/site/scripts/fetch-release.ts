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

function newerVersion(version: bigint[], previous: bigint[] | null): boolean {
  if (!previous) return true;
  for (let i = 0; i < version.length; i++) {
    if (version[i] !== previous[i]) return version[i]! > previous[i]!;
  }
  return false;
}

function releaseVersion(
  release: GitHubRelease,
  tagPattern: RegExp,
  prerelease: boolean,
): bigint[] | null {
  if (release.draft || Boolean(release.prerelease) !== prerelease) return null;
  const match = tagPattern.exec(release.tag_name ?? "");
  return match ? match.slice(1).map((part) => BigInt(part)) : null;
}

function latestRelease(
  list: GitHubRelease[],
  tagPattern: RegExp,
  prerelease = false,
): Release | null {
  let latest: GitHubRelease | null = null;
  let latestVersion: bigint[] | null = null;
  for (const release of list) {
    const version = releaseVersion(release, tagPattern, prerelease);
    if (!version) continue;
    // GitHub's release order is not semantic version order. Compare each
    // numeric component in precedence order.
    if (newerVersion(version, latestVersion)) {
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

export function latestDesktopRelease(list: GitHubRelease[]): Release | null {
  // Desktop graduated out of alpha and now ships stable releases.
  return latestRelease(list, /^desktop-v(\d+)\.(\d+)\.(\d+)$/);
}

interface StampedReleases {
  tag: string | null;
  url: string | null;
  publishedAt: string | null;
  desktop: Release;
}

const EMPTY: Release = { tag: null, url: null, publishedAt: null };

async function main() {
  try {
    const githubToken = process.env.GITHUB_TOKEN;
    const list: GitHubRelease[] = [];
    // Match the installer's bounded recent window; any later page can contain
    // a higher version even when both streams already have a candidate.
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
      desktop: latestDesktopRelease(list) ?? { ...EMPTY },
    };
    await writeFile(OUT, JSON.stringify(stamped, null, 2) + "\n");
    console.log(`fetch-release: stamped ${release.tag} + ${stamped.desktop.tag ?? "no-desktop"}`);
  } catch (err) {
    // Keep a previously stamped file if one exists; otherwise omit the badges.
    try {
      await readFile(OUT, "utf8");
      console.warn(`fetch-release: fetch failed (${err}); keeping existing release.json`);
    } catch {
      const empty: StampedReleases = { ...EMPTY, desktop: { ...EMPTY } };
      await writeFile(OUT, JSON.stringify(empty, null, 2) + "\n");
      console.warn(`fetch-release: fetch failed (${err}); badges omitted this build`);
    }
  }
}

if (import.meta.main) await main();
