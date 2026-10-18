import { describe, expect, test } from "bun:test";
import { ROOT } from "../lib/repo.ts";
import { living, parseSnapshot, trackDescendants } from "./processes.ts";
import { UsageError } from "../lib/command.ts";
import { parseOptions } from "./runner.ts";

describe("parseOptions", () => {
  test("defaults are the bounded pilot", () => {
    const options = parseOptions([]);
    expect(options).toMatchObject({
      file: [],
      package: [],
      limit: 8,
      jobs: 1,
      buildTimeout: 300,
      testTimeout: 60,
      runTimeout: 1800,
      list: false,
      root: ROOT,
    });
  });

  test("repeatable scopes keep their order", () => {
    const options = parseOptions(["--file", "a.rs", "--package", "p", "--file", "b.rs"]);
    expect(options?.file).toEqual(["a.rs", "b.rs"]);
    expect(options?.package).toEqual(["p"]);
  });

  test("bounds are positive integers and jobs is 1-4", () => {
    expect(() => parseOptions(["--limit", "0"])).toThrow(UsageError);
    expect(() => parseOptions(["--limit", "2.5"])).toThrow("invalid int value");
    expect(() => parseOptions(["--jobs", "5"])).toThrow("invalid choice");
    expect(() => parseOptions(["--unknown"])).toThrow("Unknown option");
    expect(parseOptions(["--jobs", "4"])?.jobs).toBe(4);
  });
});

describe("process tracking", () => {
  const snapshot = parseSnapshot(
    [
      "  10     1 Sat Oct 17 10:00:00 2026",
      "  11    10 Sat Oct 17 10:00:01 2026",
      "  12    11 Sat Oct 17 10:00:02 2026",
      "  13     1 Sat Oct 17 10:00:03 2026",
      "",
    ].join("\n"),
  );

  test("descendants of a living owned process are adopted transitively", () => {
    const owned = new Map([[10, "Sat Oct 17 10:00:00 2026"]]);
    trackDescendants(owned, snapshot);
    expect([...owned.keys()].toSorted((a, b) => a - b)).toEqual([10, 11, 12]);
  });

  test("a reused PID with another start time is not the owned process", () => {
    const owned = new Map([[10, "Fri Oct 16 09:00:00 2026"]]);
    expect(living(owned, snapshot).size).toBe(0);
    trackDescendants(owned, snapshot);
    expect([...owned.keys()]).toEqual([10]);
  });
});
