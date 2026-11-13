import { describe, expect, test } from "bun:test";
import { Classifier } from "./classify.ts";
import { reusedMacos, runeIos, runeMacosPlan } from "./rune-macos.ts";

const classifier = new Classifier();

describe("the Rune iOS simulator leg", () => {
  const closure = classifier.classify(["crates/phux-protocol/src/lib.rs"]);
  const direct = classifier.classify(["clients/rune/src/main.rs"]);
  const shared = classifier.classify(["Cargo.toml"]);

  test("a pull request reached only through Cargo skips the decision once the merge queue is on", () => {
    expect(runeIos(closure, "pull_request", true)).toBe(false);
    expect(runeMacosPlan(closure, "pull_request", true, "pull_request")).toMatchObject({
      macos: false,
      ios: false,
      coverage: "",
    });
  });

  test("without the queue the decision still includes it, and a pull request still does not launch macOS", () => {
    expect(runeIos(closure, "pull_request")).toBe(true);
    expect(runeMacosPlan(closure, "pull_request", false, "pull_request")).toMatchObject({
      macos: false,
      ios: false,
      coverage: "",
    });
  });

  test("a direct Rune route keeps the decision on a pull request and still does not launch macOS", () => {
    for (const c of [direct, shared, classifier.classify([])]) {
      expect(runeIos(c, "pull_request", true)).toBe(true);
      expect(runeMacosPlan(c, "pull_request", true, "pull_request").macos).toBe(false);
    }
  });

  test("main policy always runs both legs, and nothing without Rune does", () => {
    expect(runeIos(closure, "push", true)).toBe(true);
    const run = runeMacosPlan(closure, "push", true, "push");
    expect(run).toMatchObject({ macos: true, ios: true });
    expect(run.coverage.split(",")).toEqual(["macos", "rune_ios"]);
    const docs = classifier.classify(["docs/SETUP.md"]);
    expect(runeMacosPlan(docs, "push", true, "push")).toMatchObject({
      macos: false,
      ios: false,
      coverage: "",
    });
  });

  test("a main push never reuses a pull request receipt for macOS", () => {
    const main = runeMacosPlan(closure, "push").coverage;
    expect(main.split(",")).toContain("rune_ios");
    expect(runeMacosPlan(closure, "pull_request").coverage).not.toBe(main);
    expect(runeMacosPlan(closure, "pull_request", true).coverage).not.toContain("macos");
  });

  test("a reused main tree launches no macOS leg and keeps the coverage it matched", () => {
    const planned = runeMacosPlan(direct, "push");
    expect(reusedMacos(planned)).toMatchObject({ macos: false, ios: false });
    expect(planned.coverage).toContain("rune_ios");
  });

  test("a nightly or dispatch of the whole tree runs both legs", () => {
    const full = classifier.classify([]);
    expect(runeMacosPlan(full, "push", true, "push")).toMatchObject({ macos: true, ios: true });
  });
});
