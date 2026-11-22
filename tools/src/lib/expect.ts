// Contract checks that report every broken expectation, not just the first.
import { status } from "./output.ts";
import { deepEqual } from "./value.ts";

export class Expectations {
  readonly failures: string[] = [];

  that(holds: boolean, message: string): void {
    if (!holds) this.failures.push(message);
  }

  /** Strict (Object.is) equality, or structural equality for tables and lists. */
  equal(actual: unknown, expected: unknown, message: string): void {
    const holds =
      typeof expected === "object" && expected !== null
        ? deepEqual(actual, expected)
        : Object.is(actual, expected);
    this.that(holds, `${message} (expected ${show(expected)}, found ${show(actual)})`);
  }
}

function show(value: unknown): string {
  return value === undefined ? "nothing" : JSON.stringify(value);
}

/** Print each failure, or `ok` when there are none; returns the exit code. */
export function report(failures: readonly string[], ok: string): number {
  for (const failure of failures) status("error", failure);
  if (failures.length > 0) return 1;
  status("ok", ok);
  return 0;
}
