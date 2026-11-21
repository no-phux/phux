/** The status source data (spec §1.3.1): tone_of and ICU plurals. */

export interface ToneInput {
  agent?: "Working" | "Blocked" | "Idle" | "Done" | "Unknown" | null;
  attention?: boolean;
  asked?: boolean;
  exit?: number | null;
  link?: "ok" | "lost" | null;
  crashed?: boolean;
  seen?: boolean;
}
export type ToneState =
  | "needs_you"
  | "failed"
  | "lost"
  | "done"
  | "done_seen"
  | "working"
  | "idle"
  | "unknown"
  | "ended";

/**
 * The one mapping from pane facts to a status-ladder state. A pane whose transport is gone is
 * `lost`; a dead pane (crashed or nonzero exit) is `failed`; a clean exit is `done` with an agent
 * and `ended` without one; only a live pane can need you.
 */
export function toneOf(i: ToneInput): ToneState {
  const agent = i.agent ?? "Unknown";
  if (i.link === "lost") return "lost";
  if (i.crashed || (i.exit != null && i.exit !== 0)) return "failed";
  if (i.exit === 0) return agent === "Unknown" ? "ended" : i.seen ? "done_seen" : "done";
  if (agent === "Blocked" || i.attention || i.asked) return "needs_you";
  if (agent === "Done") return i.seen ? "done_seen" : "done";
  if (agent === "Working") return "working";
  if (agent === "Idle") return "idle";
  return "unknown";
}

/** English CLDR plural categories: `one` for exactly 1, `other` otherwise. */
export function plural(message: string, count: number): string {
  const m = /^\{count, plural, (.*)\}$/.exec(message);
  if (!m) throw Error(`not an ICU plural: ${message}`);
  const forms = Object.fromEntries(
    [...m[1]!.matchAll(/(=\d+|\w+) \{([^}]*)\}/g)].map((x) => [x[1], x[2]]),
  );
  const form = forms[`=${count}`] ?? (count === 1 ? forms.one : undefined) ?? forms.other;
  if (form === undefined) throw Error(`plural has no other form: ${message}`);
  return form.replaceAll("#", String(count));
}
