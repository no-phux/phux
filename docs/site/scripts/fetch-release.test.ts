import { describe, expect, test } from "bun:test";
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
