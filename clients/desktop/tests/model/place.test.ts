import { expect, test } from "bun:test";
import { groupByProject, placeFor } from "../../src/workspace/place";

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
