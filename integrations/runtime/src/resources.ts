import {
  integer,
  nullableString,
  oneOf,
  record,
  SchemaValidationError,
  string,
  type RecordValue,
} from "./validate.js";

/** Kinds `phux ls --json` names today (crates/phux-core/src/session_list.rs). */
export const RESOURCE_KINDS = ["terminal", "agent_session", "accessibility_tree", "pixel_surface", "document", "unknown"] as const;
export type KnownResourceKind = typeof RESOURCE_KINDS[number];
/** The kinds MCP `phux_documents` lists: trees, surfaces and typed documents. */
export const DOCUMENT_KINDS = ["accessibility_tree", "pixel_surface", "document"] as const;

export interface ResourceExit {
  readonly status: number | null;
  readonly signal: number | null;
  readonly reason: string | null;
  readonly exited_at_ms: number;
  readonly retained_until_ms: number;
}

/** One row of `phux ls --json` `resources`, for a kind this runtime knows. */
export interface KnownResource {
  readonly id: string;
  readonly kind: KnownResourceKind;
  readonly name: string | null;
  readonly parent: string | null;
  readonly lifecycle: string;
  readonly exit: ResourceExit | null;
  readonly producer?: string;
  readonly control?: string;
  readonly key: string | null;
}

/** A newer kind: only its identity is interpreted; nothing else is guessed. */
export interface OpaqueResource {
  readonly id: string;
  readonly kind: string;
  readonly name: string | null;
  readonly parent: string | null;
  readonly opaque: true;
}

export type ResourceEntry = KnownResource | OpaqueResource;

/** The MCP `phux_documents` entry shape, projected from the inventory. */
export interface DocumentEntry {
  readonly id: string;
  readonly kind: typeof DOCUMENT_KINDS[number];
  readonly parent: string | null;
  readonly name: string | null;
  readonly producer: string | null;
  readonly control: string | null;
}

/** `phux agent tree snapshot|follow --json`: the newest document, null before the first publish. */
export interface TreeDocument {
  readonly schema_version: number;
  readonly seq: number;
  readonly document: unknown;
}

/** MCP `phux_tree_wait`'s shape: the matching document, or why none matched. */
export type TreeWaitResult =
  | (TreeDocument & { readonly outcome: "matched" })
  | { readonly schema_version: number; readonly outcome: "closed" | "timed_out" };

/** `phux agent surface snapshot --out PATH --json`: facts about the frame written to `out`. */
export interface SurfaceFacts {
  readonly schema_version: number;
  readonly seq: number;
  readonly bytes: number;
  readonly format: "png" | "jpeg" | null;
  readonly width: number | null;
  readonly height: number | null;
  readonly content_type: string | null;
  readonly out: string | null;
}

const SELECTOR = /^(?:[^/\s]+\/)?@\d+$/;
const I32 = [-2_147_483_648, 2_147_483_647] as const;

function selector(value: unknown, path: string): string {
  const id = string(value, path);
  if (!SELECTOR.test(id)) throw new SchemaValidationError(path, "a canonical selector such as @3 or host/@3");
  return id;
}

function optionalString(row: RecordValue, key: string, path: string): Record<string, string> {
  return row[key] === undefined || row[key] === null ? {} : { [key]: string(row[key], `${path}.${key}`) };
}

function nullableInteger(value: unknown, path: string, min: number, max: number): number | null {
  return value === null || value === undefined ? null : integer(value, path, min, max);
}

function parseExit(value: unknown, path: string): ResourceExit | null {
  if (value === null || value === undefined) return null;
  const exit = record(value, path);
  return {
    status: nullableInteger(exit.status, `${path}.status`, ...I32),
    signal: nullableInteger(exit.signal, `${path}.signal`, ...I32),
    reason: exit.reason === undefined ? null : nullableString(exit.reason, `${path}.reason`),
    exited_at_ms: integer(exit.exited_at_ms, `${path}.exited_at_ms`, 0),
    retained_until_ms: integer(exit.retained_until_ms, `${path}.retained_until_ms`, 0),
  };
}

function parseResource(value: unknown, path: string): ResourceEntry {
  const row = record(value, path);
  const id = selector(row.id, `${path}.id`);
  const kind = string(row.kind, `${path}.kind`);
  if (kind.trim().length === 0) throw new SchemaValidationError(`${path}.kind`, "non-empty");
  const parent = row.parent === undefined || row.parent === null ? null : selector(row.parent, `${path}.parent`);
  const name = row.name === undefined ? null : nullableString(row.name, `${path}.name`);
  if (!(RESOURCE_KINDS as readonly string[]).includes(kind)) return { id, kind, name, parent, opaque: true };
  return {
    id,
    kind: kind as KnownResourceKind,
    name,
    parent,
    lifecycle: row.lifecycle === undefined ? "running" : string(row.lifecycle, `${path}.lifecycle`),
    exit: parseExit(row.exit, `${path}.exit`),
    ...optionalString(row, "producer", path),
    ...optionalString(row, "control", path),
    key: row.key === undefined ? null : nullableString(row.key, `${path}.key`),
  };
}

/** Parse `phux ls --json` `resources`; unknown kinds stay as opaque identity rows. */
export function parseResources(value: unknown, path = "$.resources"): ResourceEntry[] {
  if (!Array.isArray(value)) throw new SchemaValidationError(path, "an array");
  return value.map((item, index) => parseResource(item, `${path}[${index}]`));
}

export function isDocumentKind(kind: string): kind is DocumentEntry["kind"] {
  return (DOCUMENT_KINDS as readonly string[]).includes(kind);
}

export function documentEntries(resources: readonly ResourceEntry[]): DocumentEntry[] {
  return resources.flatMap((resource) => "opaque" in resource || !isDocumentKind(resource.kind) ? [] : [{
    id: resource.id,
    kind: resource.kind,
    parent: resource.parent,
    name: resource.name,
    producer: resource.producer ?? null,
    control: resource.control ?? null,
  }]);
}

export function parseTreeDocument(value: unknown): TreeDocument {
  const root = record(value, "$ (phux agent tree --json CLI shape)");
  if (!("document" in root)) throw new SchemaValidationError("$.document", "present (null before the first publish)");
  return {
    schema_version: integer(root.schema_version, "$.schema_version", 1),
    seq: integer(root.seq, "$.seq", 0),
    document: root.document,
  };
}

export function parseSurfaceFacts(value: unknown): SurfaceFacts {
  const root = record(value, "$ (phux agent surface snapshot --json CLI shape)");
  return {
    schema_version: integer(root.schema_version, "$.schema_version", 1),
    seq: integer(root.seq, "$.seq", 0),
    bytes: integer(root.bytes, "$.bytes", 0),
    format: root.format === null || root.format === undefined ? null : oneOf(root.format, "$.format", ["png", "jpeg"] as const),
    width: nullableInteger(root.width, "$.width", 0, 4_294_967_295),
    height: nullableInteger(root.height, "$.height", 0, 4_294_967_295),
    content_type: root.content_type === undefined ? null : nullableString(root.content_type, "$.content_type"),
    out: root.out === undefined ? null : nullableString(root.out, "$.out"),
  };
}
