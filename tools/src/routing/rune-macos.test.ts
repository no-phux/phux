import { describe, expect, test } from "bun:test";
import { Classifier } from "./classify.ts";
import { reusedMacos, runeMacosPlan } from "./rune-macos.ts";

const classifier = new Classifier();

describe("the Rune iOS simulator leg", () => {
  const closure = classifier.classify(["crates/phux-protocol/src/lib.rs"]);
  const direct = classifier.classify(["clients/rune/src/main.rs"]);

  test("a pull request never claims Apple coverage, even when native inputs moved", () => {
    expect(runeMacosPlan(closure, "pull_request")).toMatchObject({
      macos: false,
      ios: false,
      coverage: "",
    });
  });

  test("main policy always runs both legs, and nothing without Rune does", () => {
    const run = runeMacosPlan(closure, "push");
    expect(run).toMatchObject({ macos: true, ios: true });
    expect(run.coverage.split(",")).toEqual(["macos", "rune_ios"]);
    const docs = classifier.classify(["docs/SETUP.md"]);
    expect(runeMacosPlan(docs, "push")).toMatchObject({
      macos: false,
      ios: false,
      coverage: "",
    });
  });

  test("a reused main tree launches no macOS leg and keeps the coverage it matched", () => {
    const planned = runeMacosPlan(direct, "push");
    expect(reusedMacos(planned)).toMatchObject({ macos: false, ios: false });
    expect(planned.coverage).toContain("rune_ios");
  });

  test("a nightly or dispatch of the whole tree runs both legs", () => {
    const full = classifier.classify([]);
    expect(runeMacosPlan(full, "push")).toMatchObject({ macos: true, ios: true });
  });
});
