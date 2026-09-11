import { describe, expect, test } from "bun:test";
import { latestCockpitRelease, latestCoreRelease } from "./fetch-release";

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
