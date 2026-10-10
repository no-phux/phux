import { expect, test } from "bun:test";
import { groupByProject, placeFor, terminalHost } from "../../src/workspace/place";

test("placeFor uses the focused directory and an explicit pick overrides it", () => {
  expect(placeFor({ host: "edge", directory: "/src/api" })).toEqual({
    host: "edge",
    directory: "/src/api",
  });
  expect(placeFor({ host: "edge", directory: "/src/api" }, { directory: "/picked" })).toEqual({
    host: "edge",
    directory: "/picked",
  });
  expect(placeFor(undefined)).toEqual({});
});

test("groupByProject shares a tag and leaves untagged sessions last", () => {
  const groups = groupByProject([
    { name: "web", project: "phux" },
    { name: "notes" },
    { name: "api", project: "phux" },
    { name: "other", project: "tools" },
  ]);
  expect(groups.map((group) => group.project)).toEqual(["phux", "tools", undefined]);
  expect(groups[0]?.sessions.map((session) => session.name)).toEqual(["web", "api"]);
});

test("groupByProject separates remote hosts inside project groups", () => {
  const groups = groupByProject([
    { name: "local", project: "phux" },
    { name: "edge", project: "phux", host: "edge:22" },
    { name: "api", project: "phux", host: "api:22" },
    { name: "notes", host: "edge:22" },
  ]);
  expect(groups.map((group) => [group.project, group.host])).toEqual([
    ["phux", "api:22"],
    ["phux", "edge:22"],
    ["phux", undefined],
    [undefined, "edge:22"],
  ]);
});

test("terminalHost extracts satellite host names", () => {
  expect(terminalHost("local:1")).toBeUndefined();
  expect(terminalHost("satellite:edge:22:9")).toBe("edge:22");
  expect(terminalHost("satellite::9")).toBeUndefined();
});
