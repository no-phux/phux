/** Primitive validators shared by the CLI result parsers. */
export class SchemaValidationError extends Error {
  constructor(readonly path: string, expectation: string) {
    super(`${path} must be ${expectation}`);
    this.name = "SchemaValidationError";
  }
}

export type RecordValue = Record<string, unknown>;

export function record(value: unknown, path: string): RecordValue {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new SchemaValidationError(path, "an object");
  }
  return value as RecordValue;
}

export function string(value: unknown, path: string): string {
  if (typeof value !== "string") throw new SchemaValidationError(path, "a string");
  return value;
}

export function boolean(value: unknown, path: string): boolean {
  if (typeof value !== "boolean") throw new SchemaValidationError(path, "a boolean");
  return value;
}

export function nullableString(value: unknown, path: string): string | null {
  return value === null ? null : string(value, path);
}

export function numberInRange(value: unknown, path: string, min: number, max: number): number {
  if (typeof value !== "number" || !Number.isFinite(value) || value < min || value > max) {
    throw new SchemaValidationError(path, `a number from ${min} through ${max}`);
  }
  return value;
}

export function oneOf<const T extends readonly string[]>(value: unknown, path: string, values: T): T[number] {
  if (typeof value !== "string" || !values.includes(value)) {
    throw new SchemaValidationError(path, values.map((item) => JSON.stringify(item)).join(", "));
  }
  return value as T[number];
}

export function integer(value: unknown, path: string, min: number, max = Number.MAX_SAFE_INTEGER): number {
  if (!Number.isSafeInteger(value) || (value as number) < min || (value as number) > max) {
    throw new SchemaValidationError(path, `an integer from ${min} through ${max}`);
  }
  return value as number;
}

export function strings(value: unknown, path: string): string[] {
  if (!Array.isArray(value)) throw new SchemaValidationError(path, "an array of strings");
  return value.map((item, index) => string(item, `${path}[${index}]`));
}
