#!/usr/bin/env bun
/**
 * fetch-release.ts — stamp the site with the latest phux release.
 *
 * Fetches the newest GitHub release for no-phux/phux and writes
 * src/lib/release.json, which index.astro imports at build time (the version
 * badge next to the install command). Runs as part of `bun run build`, so the
 * badge is always current with the deployed build.
 *
 * Freshness is driven by CI: site-deploy.yml fires on release:published and
 * rebuilds, so a new release ships the badge within minutes. The weekly cron
 * in that workflow is the backstop.
 *
 * Failure policy: the badge is decorative — if GitHub is unreachable or rate
 * limits the build, keep any previously generated release.json, else write
 * { "tag": null } and let the page omit the badge. Never fail the build.
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
    const release = latestCoreRelease((await res.json()) as GitHubRelease[]);
    if (!release) throw new Error("no core phux release found in recent releases");
    await writeFile(OUT, JSON.stringify(release, null, 2) + "\n");
    console.log(`fetch-release: stamped ${release.tag}`);
  } catch (err) {
    // Keep a previously stamped file if one exists; otherwise omit the badge.
    try {
      await readFile(OUT, "utf8");
      console.warn(`fetch-release: fetch failed (${err}); keeping existing release.json`);
    } catch {
      const empty: Release = { tag: null, url: null, publishedAt: null };
      await writeFile(OUT, JSON.stringify(empty, null, 2) + "\n");
      console.warn(`fetch-release: fetch failed (${err}); badge omitted this build`);
    }
  }
}

if (import.meta.main) await main();
