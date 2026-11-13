// Which macOS legs one rune-macos.yml run launches.
//
// ci.yml no longer runs these. A pull request never launches them: the
// workflow still reports a passing `rune-macos` check so the merge queue can
// require that name without holding the pull request. A push or a merge group
// launches what the decision below names, and the receipt covers only that.
// `runeIos` is the old ci.yml rule, kept here: with the merge queue on, a
// pull request that reaches Rune only through Cargo leaves iOS to the queue.
import type { Classification } from "./classify.ts";
import type { Trigger } from "./plan.ts";

export type RuneEvent = "pull_request" | "push";

/**
 * Whether the iOS simulator leg is part of this event's decision.
 *
 * A push (and a merge group, which plans as one) runs it whenever Rune is
 * needed. A pull request runs it unless the merge queue is on and Rune was
 * reached only through the Cargo closure.
 */
export function runeIos(c: Classification, trigger: Trigger, mergeQueue = false): boolean {
  const push = trigger === "push";
  return c.needed.has("rune") && (push || !mergeQueue || c.runeDirect);
}

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

export function runeMacosPlan(
  c: Classification,
  trigger: Trigger,
  mergeQueue = false,
  event: RuneEvent = trigger,
): RuneMacosPlan {
  const ios = runeIos(c, trigger, mergeQueue);
  if (event === "pull_request") return { macos: false, ios: false, coverage: "" };
  const macos = c.needed.has("rune");
  const ranIos = macos && ios;
  return {
    macos,
    ios: ranIos,
    coverage: macos ? ["macos", ...(ranIos ? ["rune_ios"] : [])].join(",") : "",
  };
}

/** A push that already has a matching receipt launches nothing. Coverage stays. */
export function reusedMacos(p: RuneMacosPlan): RuneMacosPlan {
  return { ...p, macos: false, ios: false };
}
