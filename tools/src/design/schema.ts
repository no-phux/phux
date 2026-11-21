/**
 * A dependency-free JSON Schema 2020-12 validator for the keywords phux.tokens.schema.json uses:
 * type, const, enum, pattern, minimum, maximum, minItems, maxItems, items, properties, required,
 * additionalProperties, propertyNames, anyOf, $ref (local #/$defs/...). Anything else throws, so the
 * schema can't silently rely on an unchecked keyword.
 */
const KNOWN = new Set([
  "$schema",
  "$id",
  "$defs",
  "$ref",
  "title",
  "description",
  "type",
  "const",
  "enum",
  "pattern",
  "minimum",
  "maximum",
  "minItems",
  "maxItems",
  "items",
  "properties",
  "required",
  "additionalProperties",
  "propertyNames",
  "anyOf",
]);

type Schema = Record<string, any> | boolean;

function typeOf(v: unknown): string {
  if (v === null) return "null";
  if (Array.isArray(v)) return "array";
  if (typeof v === "number") return Number.isInteger(v) ? "integer" : "number";
  return typeof v;
}

export function validate(root: Record<string, any>, value: unknown): string[] {
  const errors: string[] = [];
  const resolveRef = (ref: string): Schema => {
    const m = /^#\/\$defs\/([\w-]+)$/.exec(ref);
    if (!m || !(m[1]! in (root.$defs ?? {}))) throw Error(`schema: unresolved $ref ${ref}`);
    return root.$defs[m[1]!];
  };
  function check(schema: Schema, v: unknown, path: string): boolean {
    if (schema === true) return true;
    if (schema === false) {
      errors.push(`${path}: not allowed`);
      return false;
    }
    for (const k of Object.keys(schema))
      if (!KNOWN.has(k)) throw Error(`schema: unsupported keyword ${k} at ${path}`);
    const before = errors.length;
    if (schema.$ref) check(resolveRef(schema.$ref), v, path);
    if (schema.type) {
      const types = ([] as string[]).concat(schema.type),
        t = typeOf(v);
      if (!types.includes(t) && !(t === "integer" && types.includes("number")))
        errors.push(`${path}: expected ${types.join("|")}, got ${t}`);
    }
    if ("const" in schema && JSON.stringify(schema.const) !== JSON.stringify(v))
      errors.push(`${path}: expected ${JSON.stringify(schema.const)}`);
    if (schema.enum && !schema.enum.some((e: unknown) => JSON.stringify(e) === JSON.stringify(v)))
      errors.push(`${path}: ${JSON.stringify(v)} not in enum`);
    if (typeof v === "string" && schema.pattern && !new RegExp(schema.pattern, "u").test(v))
      errors.push(`${path}: ${JSON.stringify(v)} does not match ${schema.pattern}`);
    if (typeof v === "number") {
      if (schema.minimum !== undefined && v < schema.minimum)
        errors.push(`${path}: ${v} < ${schema.minimum}`);
      if (schema.maximum !== undefined && v > schema.maximum)
        errors.push(`${path}: ${v} > ${schema.maximum}`);
    }
    if (Array.isArray(v)) {
      if (schema.minItems !== undefined && v.length < schema.minItems)
        errors.push(`${path}: fewer than ${schema.minItems} items`);
      if (schema.maxItems !== undefined && v.length > schema.maxItems)
        errors.push(`${path}: more than ${schema.maxItems} items`);
      if (schema.items !== undefined) v.forEach((x, i) => check(schema.items, x, `${path}[${i}]`));
    }
    if (v && typeof v === "object" && !Array.isArray(v)) {
      const o = v as Record<string, unknown>,
        props = schema.properties ?? {};
      for (const k of schema.required ?? []) if (!(k in o)) errors.push(`${path}: missing ${k}`);
      for (const [k, x] of Object.entries(o)) {
        if (schema.propertyNames) check(schema.propertyNames, k, `${path}{${k}}`);
        if (k in props) check(props[k], x, `${path}.${k}`);
        else if (schema.additionalProperties !== undefined)
          check(schema.additionalProperties, x, `${path}.${k}`);
      }
    }
    if (schema.anyOf) {
      const mark = errors.length;
      const ok = schema.anyOf.some((s: Schema) => {
        const n = errors.length;
        const pass = check(s, v, path) && errors.length === n;
        errors.length = n;
        return pass;
      });
      errors.length = mark;
      if (!ok) errors.push(`${path}: matches none of anyOf`);
    }
    return errors.length === before;
  }
  check(root, value, "$");
  return errors;
}
