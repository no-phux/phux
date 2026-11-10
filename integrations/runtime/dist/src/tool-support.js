export const MAX_MODEL_BYTES = 12 * 1024;
export const MAX_MODEL_LINES = 200;
export const DEFAULT_SHORT_TIMEOUT_MS = 10_000;
export const TARGET = { type: "string", minLength: 2, maxLength: 512, pattern: "^(?:[^/\\s]+/)?@[0-9]+$", description: "Exact @N or host/@N from phux_panes; otherwise the session-selected target. Never human focus." };
export const STRING = { type: "string", minLength: 1, maxLength: 65_536 };
export const LOCAL_TIMEOUT = { type: "integer", minimum: 1, maximum: 86_405_000, description: "Local deadline in milliseconds. Cancellation stops observation, not the terminal process or already-sent input." };
export const TIMEOUT = { type: "integer", minimum: 1, maximum: 86_400, description: "Finite observation deadline in seconds; default 30. Timeout is not completion." };
export function resolveTarget(explicit, runtime, context) {
    const target = explicit ?? runtime.getSelectedTarget(context) ?? runtime.environmentTarget;
    if (target === undefined)
        throw new Error("No phux target. Use phux_panes and pass an exact target, or spawn a terminal when a pane is needed.");
    return exactTarget(target);
}
export function exactTarget(target) {
    if (!/^(?:[^/\s]+\/)?@\d+$/.test(target))
        throw new Error("Use an exact @N or host/@N from phux_panes; session, tag and focus selectors are not safe control targets.");
    return target;
}
export function writeTarget(explicit, runtime, context) {
    const target = resolveTarget(explicit, runtime, context);
    const normalized = target.replace(/@(0+)(?=\d)/, "@");
    const parent = runtime.parentTarget?.replace(/@(0+)(?=\d)/, "@");
    if (parent !== undefined && normalized === parent)
        throw new Error("Refusing input into the pane hosting this agent. Use another exact target for phux input; ordinary commands belong in the built-in shell.");
    return target;
}
export function execution(input, context) {
    const timeoutMs = input.local_timeout_ms ?? DEFAULT_SHORT_TIMEOUT_MS;
    if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 86_405_000)
        throw new RangeError("local_timeout_ms must be between 1 and 86405000");
    return { timeoutMs, ...(context.signal === undefined ? {} : { signal: context.signal }) };
}
export function operation(input, context) {
    const seconds = input.timeout_seconds ?? 30;
    if (!Number.isSafeInteger(seconds) || seconds < 1 || seconds > 86_400)
        throw new RangeError("timeout_seconds must be between 1 and 86400");
    return { ...execution({ ...input, local_timeout_ms: input.local_timeout_ms ?? seconds * 1000 + 5000 }, context), phuxTimeoutSeconds: seconds };
}
export function schema(properties, required = []) {
    return { type: "object", properties, required, additionalProperties: false };
}
export function documentResult(operation, document, target) {
    const output = boundedResult(`${operation}${target === undefined ? "" : ` target=${target}`}`, JSON.stringify(document, null, 2));
    // Large inventory/diagnostic documents must not bypass the model cap through metadata.
    return { content: output.text, metadata: { operation, ...(target === undefined ? {} : { target }), modelOutputTruncated: output.truncated, ...(output.truncated ? {} : { result: document }) } };
}
/** Bound terminal text by UTF-8 bytes and lines; preserve the result header and truncation notices. */
export function boundedResult(header, body, phuxTruncated = false) {
    const notice = "[phux adapter truncated output; request a narrower observation]";
    const phuxNotice = "[phux reported that terminal output was already truncated]";
    const suffix = phuxTruncated ? `\n${phuxNotice}` : "";
    const budget = Math.max(0, MAX_MODEL_BYTES - Buffer.byteLength(header + suffix + notice, "utf8") - 3);
    const lines = body.split("\n");
    const lineBudget = Math.max(0, MAX_MODEL_LINES - header.split("\n").length - 3);
    let text = lines.slice(-lineBudget).join("\n");
    let truncated = lines.length > lineBudget;
    const bytes = Buffer.from(text, "utf8");
    if (bytes.length > budget) {
        let start = bytes.length - budget;
        while (start < bytes.length && (bytes[start] & 0xc0) === 0x80)
            start++;
        text = bytes.subarray(start).toString("utf8");
        truncated = true;
    }
    return { text: `${header}\n${text}${truncated ? `\n${notice}` : ""}${suffix}`, truncated };
}
