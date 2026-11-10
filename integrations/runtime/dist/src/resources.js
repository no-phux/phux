import { integer, nullableString, oneOf, record, SchemaValidationError, string, } from "./validate.js";
/** Kinds `phux ls --json` names today (crates/phux-core/src/session_list.rs). */
export const RESOURCE_KINDS = ["terminal", "agent_session", "accessibility_tree", "pixel_surface", "document", "unknown"];
/** The kinds MCP `phux_documents` lists: trees, surfaces and typed documents. */
export const DOCUMENT_KINDS = ["accessibility_tree", "pixel_surface", "document"];
const SELECTOR = /^(?:[^/\s]+\/)?@\d+$/;
const I32 = [-2_147_483_648, 2_147_483_647];
function selector(value, path) {
    const id = string(value, path);
    if (!SELECTOR.test(id))
        throw new SchemaValidationError(path, "a canonical selector such as @3 or host/@3");
    return id;
}
function optionalString(row, key, path) {
    return row[key] === undefined || row[key] === null ? {} : { [key]: string(row[key], `${path}.${key}`) };
}
function nullableInteger(value, path, min, max) {
    return value === null || value === undefined ? null : integer(value, path, min, max);
}
function parseExit(value, path) {
    if (value === null || value === undefined)
        return null;
    const exit = record(value, path);
    return {
        status: nullableInteger(exit.status, `${path}.status`, ...I32),
        signal: nullableInteger(exit.signal, `${path}.signal`, ...I32),
        reason: exit.reason === undefined ? null : nullableString(exit.reason, `${path}.reason`),
        exited_at_ms: integer(exit.exited_at_ms, `${path}.exited_at_ms`, 0),
        retained_until_ms: integer(exit.retained_until_ms, `${path}.retained_until_ms`, 0),
    };
}
function parseResource(value, path) {
    const row = record(value, path);
    const id = selector(row.id, `${path}.id`);
    const kind = string(row.kind, `${path}.kind`);
    if (kind.trim().length === 0)
        throw new SchemaValidationError(`${path}.kind`, "non-empty");
    const parent = row.parent === undefined || row.parent === null ? null : selector(row.parent, `${path}.parent`);
    const name = row.name === undefined ? null : nullableString(row.name, `${path}.name`);
    if (!RESOURCE_KINDS.includes(kind))
        return { id, kind, name, parent, opaque: true };
    return {
        id,
        kind: kind,
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
export function parseResources(value, path = "$.resources") {
    if (!Array.isArray(value))
        throw new SchemaValidationError(path, "an array");
    return value.map((item, index) => parseResource(item, `${path}[${index}]`));
}
export function isDocumentKind(kind) {
    return DOCUMENT_KINDS.includes(kind);
}
export function documentEntries(resources) {
    return resources.flatMap((resource) => "opaque" in resource || !isDocumentKind(resource.kind) ? [] : [{
            id: resource.id,
            kind: resource.kind,
            parent: resource.parent,
            name: resource.name,
            producer: resource.producer ?? null,
            control: resource.control ?? null,
        }]);
}
export function parseTreeDocument(value) {
    const root = record(value, "$ (phux agent tree --json CLI shape)");
    if (!("document" in root))
        throw new SchemaValidationError("$.document", "present (null before the first publish)");
    return {
        schema_version: integer(root.schema_version, "$.schema_version", 1),
        seq: integer(root.seq, "$.seq", 0),
        document: root.document,
    };
}
export function parseSurfaceFacts(value) {
    const root = record(value, "$ (phux agent surface snapshot --json CLI shape)");
    return {
        schema_version: integer(root.schema_version, "$.schema_version", 1),
        seq: integer(root.seq, "$.seq", 0),
        bytes: integer(root.bytes, "$.bytes", 0),
        format: root.format === null || root.format === undefined ? null : oneOf(root.format, "$.format", ["png", "jpeg"]),
        width: nullableInteger(root.width, "$.width", 0, 4_294_967_295),
        height: nullableInteger(root.height, "$.height", 0, 4_294_967_295),
        content_type: root.content_type === undefined ? null : nullableString(root.content_type, "$.content_type"),
        out: root.out === undefined ? null : nullableString(root.out, "$.out"),
    };
}
