import { describe, expect, test } from "bun:test";
import { copyFile, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { latestCoreRelease, latestDesktopRelease } from "./fetch-release";

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

describe("latestDesktopRelease", () => {
  test("selects a published stable desktop release, not a draft or another stream", () => {
    expect(
      latestDesktopRelease([
        { tag_name: "desktop-v0.2.0", draft: true },
        { tag_name: "v0.54.0" },
        { tag_name: "cockpit-v0.34.0" },
        { tag_name: "desktop-v0.1.0-alpha.10", prerelease: true },
        {
          tag_name: "desktop-v0.1.0",
          html_url: "https://github.com/no-phux/phux/releases/tag/desktop-v0.1.0",
          published_at: "2026-10-14T09:19:12Z",
        },
        { tag_name: "desktop-v0.0.9" },
      ]),
    ).toEqual({
      tag: "desktop-v0.1.0",
      url: "https://github.com/no-phux/phux/releases/tag/desktop-v0.1.0",
      publishedAt: "2026-10-14T09:19:12Z",
    });
  });

  test("omits the badge when no published stable desktop release exists", () => {
    expect(
      latestDesktopRelease([
        { tag_name: "desktop-v0.1.0", draft: true },
        { tag_name: "desktop-v0.1.0-alpha.1", prerelease: true },
        { tag_name: "desktop-v0.1.0-alpha.0" },
        { tag_name: "desktop-v0.1.0-extra" },
        { tag_name: "v0.54.0" },
      ]),
    ).toBeNull();
  });
});

for (const [name, selector, prefix] of [
  ["core", latestCoreRelease, "v"],
] as const) {
  describe(`${name} numeric precedence`, () => {
    test.each([
      ["patch", "1.2.9", "1.2.10"],
      ["minor", "1.9.99", "1.10.0"],
      ["major", "9.99.99", "10.0.0"],
      ["large patch", "1.0.9007199254740992", "1.0.9007199254740993"],
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
        { tag_name: "cockpit-v999.0.0" },
        { tag_name: `${prefix}1.2.3` },
      ])).toEqual({ tag: `${prefix}1.2.3`, url: null, publishedAt: null });
    });

    test("an empty list has no release", () => {
      expect(selector([])).toBeNull();
    });

    test("equal versions retain the first release metadata", () => {
      expect(selector([
        { tag_name: `${prefix}1.2.3`, html_url: "https://example.test/first" },
        { tag_name: `${prefix}1.2.3`, html_url: "https://example.test/second" },
      ])?.url).toBe("https://example.test/first");
    });
  });
}

describe("desktop numeric precedence", () => {
  test.each([
    ["patch", "0.1.9", "0.1.10"],
    ["minor", "0.9.99", "0.10.0"],
    ["major", "9.99.99", "10.0.0"],
    ["large patch", "1.0.9007199254740992", "1.0.9007199254740993"],
  ])("compares the %s numerically regardless of input order", (_component, older, newer) => {
    const releases = [
      { tag_name: `desktop-v${older}` },
      { tag_name: `desktop-v${newer}` },
    ];
    expect(latestDesktopRelease(releases)?.tag).toBe(`desktop-v${newer}`);
    expect(latestDesktopRelease([...releases].reverse())?.tag).toBe(`desktop-v${newer}`);
  });

  test("higher drafts, prereleases, and other streams cannot displace a stable release", () => {
    expect(latestDesktopRelease([
      { tag_name: "desktop-v999.0.0", draft: true },
      { tag_name: "desktop-v1000.0.0", prerelease: true },
      { tag_name: "desktop-v999.0.0-alpha.1", prerelease: true },
      { tag_name: "v999.0.0" },
      { tag_name: "cockpit-v999.0.0" },
      { tag_name: "desktop-v0.1.0" },
      { tag_name: "desktop-v0.2.0" },
    ])).toEqual({ tag: "desktop-v0.2.0", url: null, publishedAt: null });
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
      { tag_name: "desktop-v0.1.0" },
      ...Array.from({ length: 27 }, () => ({ tag_name: "other-v999.0.0" })),
    ];
    const result = await runStamp([first, [
      { tag_name: "v1.2.10" },
      { tag_name: "cockpit-v1.2.10" },
      { tag_name: "desktop-v0.1.1" },
    ]]);
    expect(result.exitCode).toBe(0);
    expect(result.requestedPages).toEqual([1, 2]);
    expect(result.stamp.tag).toBe("v1.2.10");
    expect(result.stamp).not.toHaveProperty("cockpit");
    expect(result.stamp.desktop.tag).toBe("desktop-v0.1.1");
    expect(result.stderr).toBe("");
  });

  test("stops on an empty page after a full page", async () => {
    const result = await runStamp([
      Array.from({ length: 30 }, () => ({ tag_name: "v1.0.0" })),
      [],
    ]);
    expect(result.requestedPages).toEqual([1, 2]);
    expect(result.stamp.tag).toBe("v1.0.0");
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
    const existing = '{"tag":"v0.9.0","desktop":{"tag":"desktop-v0.1.0"}}\n';
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
      desktop: { tag: null, url: null, publishedAt: null },
    });
    expect(result.stderr).toContain("badges omitted this build");
  });

  test("no core release retains the existing failure policy", async () => {
    const result = await runStamp([[{ tag_name: "desktop-v0.1.0" }]]);
    expect(result.exitCode).toBe(0);
    expect(result.requestedPages).toEqual([1]);
    expect(result.stamp.tag).toBeNull();
    expect(result.stamp.desktop.tag).toBeNull();
    expect(result.stderr).toContain("no core phux release found");
  });
});
