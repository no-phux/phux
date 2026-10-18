// The release drift check is a pure function of the GitHub and repo state it reads.
import { expect, test } from "bun:test";
import { problems, type Release, type Snapshot } from "./release-drift.ts";

const OLD = "2020-01-01T00:00:00Z";
const release = (tag: string, draft = false, assets = 2): Release => ({
  tag,
  draft,
  prerelease: false,
  assets,
  created: OLD,
});
const finished = (): Snapshot => ({
  repo: "example/repo",
  releases: [release("v1.0.0"), release("desktop-v0.5.0"), release("pi-v0.1.0")],
  pending: [],
  manifest: { ".": "1.0.0", "clients/desktop": "0.5.0", pi: "0.1.0" },
  packages: { "clients/desktop": { component: "desktop" }, pi: { component: "pi" } },
  bumped: OLD,
  stable: { products: { phux: { version: "1.0.0" }, desktop: { version: "0.5.0" } } },
});

test("a finished pipeline passes", () => {
  expect(problems(finished(), 0)).toEqual([]);
});

test.each<[string, (state: Snapshot) => Snapshot, string]>([
  [
    "an old draft names its recovery",
    (s) => ({ ...s, releases: [...s.releases, release("desktop-v9.0.0", true)] }),
    "-f tag=desktop-v9.0.0",
  ],
  [
    "a public release needs assets",
    (s) => ({ ...s, releases: [...s.releases, release("v0.0.9", false, 0)] }),
    "v0.0.9 is published with no assets",
  ],
  [
    "a merged release PR needs a release",
    (s) => ({ ...s, pending: [{ number: 7, title: "t", merged: OLD }] }),
    "#7",
  ],
  [
    "every manifest version needs a release",
    (s) => ({ ...s, releases: s.releases.slice(1) }),
    ". is at 1.0.0, but v1.0.0 has no release",
  ],
  [
    "the stable manifest names the newest release",
    (s) => ({ ...s, releases: [...s.releases, release("v9.0.0")] }),
    "but v9.0.0 is published",
  ],
  [
    "the stable manifest must be readable",
    (s) => ({ ...s, stable: "HTTP 404" }),
    "unreadable: HTTP 404",
  ],
])("%s", (_, change, message) => {
  expect(problems(change(finished()), 0).join("\n")).toContain(message);
});

test("the grace window hides fresh drafts and fresh manifest bumps", () => {
  const state = finished();
  const releases = [...state.releases, release("v1.1.0", true)];
  const fresh = { ...state, releases, manifest: { ...state.manifest, ".": "1.2.0" } };
  expect(problems(fresh, 60_000, Date.parse(OLD) + 1000)).toEqual([]);
  expect(problems(fresh, 0, Date.parse(OLD) + 1000)).toHaveLength(2);
});
