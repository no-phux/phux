// Narrowing for parsed TOML and JSON, which arrive as `unknown`.

export type Table = Record<string, unknown>;

export function isTable(value: unknown): value is Table {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function table(value: unknown): Table {
  return isTable(value) ? value : {};
}

export function str(value: unknown): string | undefined {
  return typeof value === "string" ? value : undefined;
}

export function list(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

export function strings(value: unknown): string[] {
  return list(value).filter((item): item is string => typeof item === "string");
}

/** Structural equality for parsed TOML/JSON documents. */
export function deepEqual(left: unknown, right: unknown): boolean {
  return Bun.deepEquals(left, right, true);
}
