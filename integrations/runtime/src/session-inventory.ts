import { parseResources, type ResourceEntry } from "./resources.js";
import { boolean, integer, nullableString, record, SchemaValidationError, string, strings } from "./validate.js";

export interface SessionSummary {
  readonly name: string;
  readonly windows: number;
  readonly attached: boolean;
  /** Persistent session key for typed operations (v4+), or null from a legacy server. */
  readonly key?: string | null;
}

export interface SessionList {
  readonly schema_version: 1 | 2 | 3 | 4;
  readonly sessions: readonly SessionSummary[];
  /** Canonical selectors for every addressable terminal (v2+; empty for v1). */
  readonly terminals: readonly string[];
  /** Every addressable resource of every kind (v3+; empty when absent). */
  readonly resources: readonly ResourceEntry[];
  /** One diagnostic per unreachable federation satellite; empty when absent. */
  readonly unreachable: readonly string[];
}

export function parseSessionList(value: unknown): SessionList {
  const root = record(value, "$ (phux ls --json CLI shape)");
  const schema = root.schema_version;
  if (schema !== 1 && schema !== 2 && schema !== 3 && schema !== 4) {
    throw new SchemaValidationError("$.schema_version", "a supported value (1 through 4)");
  }
  if (!Array.isArray(root.sessions)) {
    throw new SchemaValidationError("$.sessions", "an array");
  }
  const sessions = root.sessions.map((item, index) => parseSession(item, index, schema));
  // v3/v4 preserve these v2 fields; v4 also carries per-session keys, and the
  // resource inventory is additive (crates/phux-core/src/session_list.rs).
  const terminals = schema === 1 && root.terminals === undefined
    ? []
    : strings(root.terminals, "$.terminals");
  const resources = root.resources === undefined ? [] : parseResources(root.resources, "$.resources");
  const unreachable = root.unreachable === undefined ? [] : strings(root.unreachable, "$.unreachable");
  return { schema_version: schema, sessions, terminals, resources, unreachable };
}

function parseSession(item: unknown, index: number, schema: SessionList["schema_version"]): SessionSummary {
  const path = `$.sessions[${index}]`;
  const row = record(item, path);
  const name = string(row.name, `${path}.name`);
  if (name.length === 0) throw new SchemaValidationError(`${path}.name`, "non-empty");
  return {
    name,
    windows: integer(row.windows, `${path}.windows`, 0),
    attached: boolean(row.attached, `${path}.attached`),
    ...(schema === 4 && row.key !== undefined ? { key: nullableString(row.key, `${path}.key`) } : {}),
  };
}
