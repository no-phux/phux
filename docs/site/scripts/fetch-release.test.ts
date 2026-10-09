import { describe, expect, test } from "bun:test";
import { copyFile, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { latestCockpitRelease, latestCoreRelease, latestDesktopRelease } from "./fetch-release";

describe("latestCoreRelease", () => {
  test("selects the newest core release from mixed release streams", () => {
    expect(
      latestCoreRelease([
        { tag_name: "cockpit-v0.18.0" },
        { tag_name: "opencode-plugin-v0.2.2" },
        {
          tag_name: "v0.28.0",
          html_url: "https://github.com/no-phux/phux/releases/tag/v0.28.0",
          published_at: "2026-09-09T05:26:33Z",
        },
        { tag_name: "v0.27.1" },
      ]),
    ).toEqual({
      tag: "v0.28.0",
      url: "https://github.com/no-phux/phux/releases/tag/v0.28.0",
      publishedAt: "2026-09-09T05:26:33Z",
    });
  });

  test("ignores drafts, prereleases, and non-semver core tags", () => {
    expect(
      latestCoreRelease([
        { tag_name: "v1.0.0", draft: true },
        { tag_name: "v0.30.0", prerelease: true },
        { tag_name: "v0.29.0-rc.1" },
        { tag_name: "cockpit-v1.0.0" },
      ]),
    ).toBeNull();
  });
});

describe("latestCockpitRelease", () => {
  test("selects the newest cockpit release from mixed release streams", () => {
    expect(
      latestCockpitRelease([
        { tag_name: "v0.31.0" },
        { tag_name: "opencode-plugin-v0.2.2" },
        {
          tag_name: "cockpit-v0.21.0",
          html_url: "https://github.com/no-phux/phux/releases/tag/cockpit-v0.21.0",
          published_at: "2026-09-10T05:26:33Z",
        },
        { tag_name: "cockpit-v0.20.0" },
      ]),
    ).toEqual({
      tag: "cockpit-v0.21.0",
      url: "https://github.com/no-phux/phux/releases/tag/cockpit-v0.21.0",
      publishedAt: "2026-09-10T05:26:33Z",
    });
  });

  test("ignores drafts, prereleases, and core tags", () => {
    expect(
      latestCockpitRelease([
        { tag_name: "cockpit-v1.0.0", draft: true },
        { tag_name: "cockpit-v0.22.0", prerelease: true },
        { tag_name: "v0.31.0" },
      ]),
    ).toBeNull();
  });
});

describe("latestDesktopRelease", () => {
  test("selects a published alpha, not a draft or another component's release", () => {
    expect(
      latestDesktopRelease([
        { tag_name: "desktop-v0.1.0-alpha.9", draft: true, prerelease: true },
        { tag_name: "v0.54.0" },
        { tag_name: "cockpit-v0.34.0" },
        { tag_name: "desktop-v0.1.0" },
        { tag_name: "desktop-v0.1.0-beta.1", prerelease: true },
        {
          tag_name: "desktop-v0.1.0-alpha.8",
          prerelease: true,
          html_url: "https://github.com/no-phux/phux/releases/tag/desktop-v0.1.0-alpha.8",
          published_at: "2026-10-08T09:19:12Z",
        },
        { tag_name: "desktop-v0.1.0-alpha.7", prerelease: true },
      ]),
    ).toEqual({
      tag: "desktop-v0.1.0-alpha.8",
      url: "https://github.com/no-phux/phux/releases/tag/desktop-v0.1.0-alpha.8",
      publishedAt: "2026-10-08T09:19:12Z",
    });
  });

  test("omits the badge when no published desktop alpha exists", () => {
    expect(
      latestDesktopRelease([
        { tag_name: "desktop-v0.1.0-alpha.1", draft: true },
        { tag_name: "desktop-v0.1.0-alpha.0" },
        { tag_name: "desktop-v0.1.0-alpha.1-extra" },
        { tag_name: "v0.54.0" },
      ]),
    ).toBeNull();
  });
});

for (const [name, selector, prefix] of [
  ["core", latestCoreRelease, "v"],
  ["cockpit", latestCockpitRelease, "cockpit-v"],
] as const) {
  describe(`${name} numeric precedence`, () => {
    test.each([
      ["patch", "1.2.9", "1.2.10"],
      ["minor", "1.9.99", "1.10.0"],
      ["major", "9.99.99", "10.0.0"],
    ])("compares the %s numerically regardless of input order", (_component, older, newer) => {
      const releases = [
        { tag_name: `${prefix}${older}` },
        { tag_name: `${prefix}${newer}` },
      ];
      expect(selector(releases)?.tag).toBe(`${prefix}${newer}`);
      expect(selector([...releases].reverse())?.tag).toBe(`${prefix}${newer}`);
    });

    test("higher drafts, prereleases, and wrong streams cannot displace a stable release", () => {
      expect(selector([
        { tag_name: `${prefix}999.0.0`, draft: true },
        { tag_name: `${prefix}998.0.0`, prerelease: true },
        { tag_name: `${prefix}997.0.0-rc.1` },
        { tag_name: "desktop-v999.0.0-alpha.1" },
        { tag_name: prefix === "v" ? "cockpit-v999.0.0" : "v999.0.0" },
        { tag_name: `${prefix}1.2.3` },
      ])).toEqual({ tag: `${prefix}1.2.3`, url: null, publishedAt: null });
    });

    test("an empty list has no release", () => {
      expect(selector([])).toBeNull();
    });
  });
}

describe("desktop numeric precedence", () => {
  test.each([
    ["alpha", "0.1.0-alpha.9", "0.1.0-alpha.10"],
    ["patch", "0.1.9-alpha.99", "0.1.10-alpha.1"],
    ["minor", "0.9.99-alpha.99", "0.10.0-alpha.1"],
    ["major", "9.99.99-alpha.99", "10.0.0-alpha.1"],
  ])("compares the %s numerically regardless of input order", (_component, older, newer) => {
    const releases = [
      { tag_name: `desktop-v${older}`, prerelease: true },
      { tag_name: `desktop-v${newer}`, prerelease: true },
    ];
    expect(latestDesktopRelease(releases)?.tag).toBe(`desktop-v${newer}`);
    expect(latestDesktopRelease([...releases].reverse())?.tag).toBe(`desktop-v${newer}`);
  });

  test("higher drafts, other channels, and other streams cannot displace an alpha", () => {
    expect(latestDesktopRelease([
      { tag_name: "desktop-v999.0.0-alpha.1", draft: true },
      { tag_name: "desktop-v1000.0.0-alpha.1", prerelease: false },
      { tag_name: "desktop-v999.0.0" },
      { tag_name: "desktop-v999.0.0-beta.1", prerelease: true },
      { tag_name: "v999.0.0" },
      { tag_name: "cockpit-v999.0.0" },
      { tag_name: "desktop-v0.1.0-alpha.9", prerelease: true },
      { tag_name: "desktop-v0.1.0-alpha.10", prerelease: true },
    ])).toEqual({ tag: "desktop-v0.1.0-alpha.10", url: null, publishedAt: null });
  });
});

type GitHubRelease = Parameters<typeof latestCoreRelease>[0][number];

async function runStamp(
  pages: GitHubRelease[][],
  { existing, failedPage }: { existing?: string; failedPage?: number } = {},
) {
  const root = await mkdtemp(join(tmpdir(), "phux-release-stamp-"));
  try {
    await mkdir(join(root, "scripts"));
    await mkdir(join(root, "src/lib"), { recursive: true });
    const script = join(root, "scripts/fetch-release.ts");
    const out = join(root, "src/lib/release.json");
    const requests = join(root, "requests.json");
    const preload = join(root, "preload.ts");
    await copyFile(join(import.meta.dir, "fetch-release.ts"), script);
    if (existing !== undefined) await writeFile(out, existing);
    await writeFile(preload, `
      const pages = ${JSON.stringify(pages)};
      const requestedPages = [];
      globalThis.fetch = async (input) => {
        const url = new URL(input);
        if (url.origin + url.pathname !== "https://api.github.com/repos/no-phux/phux/releases"
            || url.searchParams.get("per_page") !== "30") throw new Error("unexpected API request");
        const page = Number(url.searchParams.get("page"));
        requestedPages.push(page);
        await Bun.write(${JSON.stringify(requests)}, JSON.stringify(requestedPages));
        if (page === ${failedPage ?? 0}) return new Response("unavailable", { status: 503 });
        if (!pages[page - 1]) throw new Error("unexpected page " + page);
        return Response.json(pages[page - 1]);
      };
    `);
    const child = Bun.spawn([process.execPath, "--preload", preload, script], {
      env: { ...process.env, GITHUB_TOKEN: "" },
      stdout: "pipe",
      stderr: "pipe",
    });
    const [stdout, stderr, exitCode] = await Promise.all([
      new Response(child.stdout).text(),
      new Response(child.stderr).text(),
      child.exited,
    ]);
    const raw = await readFile(out, "utf8");
    return {
      raw,
      stamp: JSON.parse(raw),
      requestedPages: JSON.parse(await readFile(requests, "utf8")),
      stdout,
      stderr,
      exitCode,
    };
  } finally {
    await rm(root, { recursive: true, force: true });
  }
}

describe("release stamp command", () => {
  test("searches later pages even when every stream already has a release", async () => {
    const first = [
      { tag_name: "v1.2.9" },
      { tag_name: "cockpit-v1.2.9" },
      { tag_name: "desktop-v0.1.0-alpha.9", prerelease: true },
      ...Array.from({ length: 27 }, () => ({ tag_name: "other-v999.0.0" })),
    ];
    const result = await runStamp([first, [
      { tag_name: "v1.2.10" },
      { tag_name: "cockpit-v1.2.10" },
      { tag_name: "desktop-v0.1.0-alpha.10", prerelease: true },
    ]]);
    expect(result.exitCode).toBe(0);
    expect(result.requestedPages).toEqual([1, 2]);
    expect(result.stamp.tag).toBe("v1.2.10");
    expect(result.stamp.cockpit.tag).toBe("cockpit-v1.2.10");
    expect(result.stamp.desktop.tag).toBe("desktop-v0.1.0-alpha.10");
    expect(result.stderr).toBe("");
  });

  test("stops on an empty page after a full page", async () => {
    const result = await runStamp([
      Array.from({ length: 30 }, () => ({ tag_name: "v1.0.0" })),
      [],
    ]);
    expect(result.requestedPages).toEqual([1, 2]);
    expect(result.stamp.tag).toBe("v1.0.0");
    expect(result.stamp.cockpit.tag).toBeNull();
    expect(result.stamp.desktop.tag).toBeNull();
    expect(result.stderr).toBe("");
  });

  test("caps the recent window at ten full pages", async () => {
    const pages = Array.from({ length: 11 }, (_, index) =>
      Array.from({ length: 30 }, () => ({ tag_name: `v1.0.${index + 1}` })));
    const result = await runStamp(pages);
    expect(result.requestedPages).toEqual([1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    expect(result.stamp.tag).toBe("v1.0.10");
    expect(result.stderr).toBe("");
  });

  test("a later-page failure keeps the previous file instead of stamping a partial list", async () => {
    const existing = '{"tag":"v0.9.0","desktop":{"tag":"desktop-v0.1.0-alpha.8"}}\n';
    const result = await runStamp([
      Array.from({ length: 30 }, () => ({ tag_name: "v1.0.0" })),
    ], { existing, failedPage: 2 });
    expect(result.exitCode).toBe(0);
    expect(result.requestedPages).toEqual([1, 2]);
    expect(result.raw).toBe(existing);
    expect(result.stderr).toContain("keeping existing release.json");
  });

  test("a fetch failure without a previous file omits all badges", async () => {
    const result = await runStamp([], { failedPage: 1 });
    expect(result.exitCode).toBe(0);
    expect(result.stamp).toEqual({
      tag: null, url: null, publishedAt: null,
      cockpit: { tag: null, url: null, publishedAt: null },
      desktop: { tag: null, url: null, publishedAt: null },
    });
    expect(result.stderr).toContain("badges omitted this build");
  });

  test("no core release retains the existing failure policy", async () => {
    const result = await runStamp([[{ tag_name: "desktop-v0.1.0-alpha.10", prerelease: true }]]);
    expect(result.exitCode).toBe(0);
    expect(result.requestedPages).toEqual([1]);
    expect(result.stamp.tag).toBeNull();
    expect(result.stamp.desktop.tag).toBeNull();
    expect(result.stderr).toContain("no core phux release found");
  });
});
