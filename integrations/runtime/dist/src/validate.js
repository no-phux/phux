/** Primitive validators shared by the CLI result parsers. */
export class SchemaValidationError extends Error {
    path;
    constructor(path, expectation) {
        super(`${path} must be ${expectation}`);
        this.path = path;
        this.name = "SchemaValidationError";
    }
}
export function record(value, path) {
    if (value === null || typeof value !== "object" || Array.isArray(value)) {
        throw new SchemaValidationError(path, "an object");
    }
    return value;
}
export function string(value, path) {
    if (typeof value !== "string")
        throw new SchemaValidationError(path, "a string");
    return value;
}
export function boolean(value, path) {
    if (typeof value !== "boolean")
        throw new SchemaValidationError(path, "a boolean");
    return value;
}
export function nullableString(value, path) {
    return value === null ? null : string(value, path);
}
export function numberInRange(value, path, min, max) {
    if (typeof value !== "number" || !Number.isFinite(value) || value < min || value > max) {
        throw new SchemaValidationError(path, `a number from ${min} through ${max}`);
    }
    return value;
}
export function oneOf(value, path, values) {
    if (typeof value !== "string" || !values.includes(value)) {
        throw new SchemaValidationError(path, values.map((item) => JSON.stringify(item)).join(", "));
    }
    return value;
}
export function integer(value, path, min, max = Number.MAX_SAFE_INTEGER) {
    if (!Number.isSafeInteger(value) || value < min || value > max) {
        throw new SchemaValidationError(path, `an integer from ${min} through ${max}`);
    }
    return value;
}
export function strings(value, path) {
    if (!Array.isArray(value))
        throw new SchemaValidationError(path, "an array of strings");
    return value.map((item, index) => string(item, `${path}[${index}]`));
}
