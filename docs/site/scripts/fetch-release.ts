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

export function latestCoreRelease(list: GitHubRelease[]): Release | null {
  // GitHub returns releases newest-first. This repository publishes multiple
  // streams; only bare vX.Y.Z tags represent the core phux CLI.
  const data = list.find(
    (release) =>
      !release.draft &&
      !release.prerelease &&
      /^v\d+\.\d+\.\d+$/.test(release.tag_name ?? ""),
  );
  if (!data?.tag_name) return null;
  return {
    tag: data.tag_name,
    url: data.html_url ?? null,
    publishedAt: data.published_at ?? null,
  };
}

export function latestCockpitRelease(list: GitHubRelease[]): Release | null {
  // The Cockpit native macOS client ships as its own stream: cockpit-vX.Y.Z.
  const data = list.find(
    (release) =>
      !release.draft &&
      !release.prerelease &&
      /^cockpit-v\d+\.\d+\.\d+$/.test(release.tag_name ?? ""),
  );
  if (!data?.tag_name) return null;
  return {
    tag: data.tag_name,
    url: data.html_url ?? null,
    publishedAt: data.published_at ?? null,
  };
}

interface StampedReleases {
  tag: string | null;
  url: string | null;
  publishedAt: string | null;
  cockpit: Release;
}

const EMPTY: Release = { tag: null, url: null, publishedAt: null };

async function main() {
  try {
    const githubToken = process.env.GITHUB_TOKEN;
    const res = await fetch(API, {
      headers: {
        Accept: "application/vnd.github+json",
        "User-Agent": "phux-site-build",
        ...(githubToken ? { Authorization: `Bearer ${githubToken}` } : {}),
      },
      signal: AbortSignal.timeout(10_000),
    });
    if (!res.ok) throw new Error(`github api ${res.status}`);
    const list = (await res.json()) as GitHubRelease[];
    const release = latestCoreRelease(list);
    if (!release) throw new Error("no core phux release found in recent releases");
    const stamped: StampedReleases = {
      ...release,
      cockpit: latestCockpitRelease(list) ?? { ...EMPTY },
    };
    await writeFile(OUT, JSON.stringify(stamped, null, 2) + "\n");
    console.log(`fetch-release: stamped ${release.tag} + ${stamped.cockpit.tag ?? "no-cockpit"}`);
  } catch (err) {
    // Keep a previously stamped file if one exists; otherwise omit the badges.
    try {
      await readFile(OUT, "utf8");
      console.warn(`fetch-release: fetch failed (${err}); keeping existing release.json`);
    } catch {
      const empty: StampedReleases = { ...EMPTY, cockpit: { ...EMPTY } };
      await writeFile(OUT, JSON.stringify(empty, null, 2) + "\n");
      console.warn(`fetch-release: fetch failed (${err}); badges omitted this build`);
    }
  }
}

if (import.meta.main) await main();
