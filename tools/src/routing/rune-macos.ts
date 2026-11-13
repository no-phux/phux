// Which macOS legs one rune-macos.yml run launches.
//
// ci.yml's Rune lane is Linux-only. A pull request never launches Apple: the
// workflow still reports a cheap passing check. The merge queue requires the
// actual native workflow through ci, not that PR placeholder. Every selected
// non-PR run qualifies BOTH Apple legs; no queue-variable exemption remains.
import type { Classification } from "./classify.ts";

export type RuneEvent = "pull_request" | "push";

export interface RuneMacosPlan {
  /** Launch the macOS build leg. Never for a pull request. */
  readonly macos: boolean;
  /** Launch the iOS simulator leg. Never for a pull request. */
  readonly ios: boolean;
  /**
   * Legs this run actually executes. A pull request is empty, so its receipt
   * cannot stand in for a main push.
   */
  readonly coverage: string;
}

export function runeMacosPlan(c: Classification, event: RuneEvent): RuneMacosPlan {
  if (event === "pull_request") return { macos: false, ios: false, coverage: "" };
  const macos = c.needed.has("rune");
  return {
    macos,
    ios: macos,
    coverage: macos ? "macos,rune_ios" : "",
  };
}

/** A push that already has a matching receipt launches nothing. Coverage stays. */
export function reusedMacos(p: RuneMacosPlan): RuneMacosPlan {
  return { ...p, macos: false, ios: false };
}
