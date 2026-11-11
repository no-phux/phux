import { PhuxError } from "./errors.js";
import { parseScreenState } from "./schemas.js";
const TOOL = {
    snapshot: "phux_snapshot reads a terminal's screen",
    wait: "phux_wait waits on a terminal's screen",
    "send-keys": "phux_send_keys types into a terminal",
    paste: "phux_paste types into a terminal",
};
const LABELS = {
    "an accessibility tree": "accessibility_tree",
    "a pixel surface": "pixel_surface",
    "a document": "document",
    "an agent session": "agent_session",
};
const ID = String.raw `((?:[^\s/'\x60]+/)?@\d+)`;
/** `crates/phux-client/src/read.rs` `kind_label`, after the id it names. */
const LABELED = new RegExp(String.raw `${ID}(?:,| is) (an accessibility tree|a pixel surface|a document|an agent session|a resource of kind (\S+?))(?=$|[\s,.:;])`);
const NOT_TEXT = new RegExp(String.raw `${ID} is an? \S+ document, not text`);
/** A document wait that timed out or closed: a tree or a typed document. */
const DOCUMENT_WAIT_END = new RegExp(String.raw `^(?:wait timed out after \d+ documents? of ${ID}|${ID} closed before a document matched)$`);
/** The server's refusal of input to a non-Terminal, which `phux paste` prints. */
const INPUT_REFUSED = /refused for '((?:[^\s/']+\/)?@\d+)': resource is ([a-z][a-z-]*), operation requires terminal/;
function refused(verb, text) {
    const labeled = LABELED.exec(text);
    if (labeled !== null)
        return { id: labeled[1], kind: labeled[3] ?? LABELS[labeled[2]] };
    const binary = NOT_TEXT.exec(text);
    if (binary !== null)
        return { id: binary[1], kind: "document" };
    const input = INPUT_REFUSED.exec(text);
    if (input !== null)
        return { id: input[1], kind: input[2].replaceAll("-", "_") };
    const ended = verb === "wait" ? DOCUMENT_WAIT_END.exec(text.trim()) : null;
    return ended === null ? undefined : { id: ended[1] ?? ended[2], kind: undefined };
}
function label(kind) {
    if (kind === undefined)
        return "an accessibility tree or a typed document";
    return Object.entries(LABELS).find(([, value]) => value === kind)?.[0] ?? `a resource of kind ${kind}`;
}
function instead(verb, kind, id) {
    if (kind === "agent_session")
        return "read an agent session with `phux agent log`; address its parent pane for the screen and for keys";
    const documentKind = kind === undefined || kind === "accessibility_tree" || kind === "pixel_surface" || kind === "document";
    if (!documentKind)
        return "address a terminal; phux_list names each resource's kind";
    if (verb === "send-keys" || verb === "paste") {
        return "keys reach a terminal only; a document takes typed intents through its producer: `phux agent page type TEXT` or `phux agent page key KEY`";
    }
    if (kind === "pixel_surface")
        return "use phux_surface_snapshot";
    if (verb === "wait")
        return "use phux_tree_wait, which matches any document's text";
    if (kind === "accessibility_tree")
        return "use phux_tree_snapshot";
    return `\`phux snapshot ${id}\` from a shell reads a typed document's content; no tool here reads it yet`;
}
function refusal(verb, found, target, details = {}) {
    const id = found.id ?? target ?? "the target";
    const message = `${TOOL[verb]}, and ${id} is ${label(found.kind)}: ${instead(verb, found.kind, id)}`;
    return new PhuxError("wrong_resource_kind", message, { ...details, ...(found.kind === undefined ? {} : { resourceKind: found.kind }) });
}
function isTerminalVerb(verb) {
    return Object.hasOwn(TOOL, verb);
}
function cliMessage(cliError) {
    const detail = cliError?.["error"];
    if (detail === null || typeof detail !== "object" || Array.isArray(detail))
        return undefined;
    const message = detail.message;
    return typeof message === "string" ? message : undefined;
}
export function commandFailed(verb, executable, args, result) {
    let cliError;
    try {
        const document = JSON.parse(result.stderr.trim());
        if (document !== null && typeof document === "object" && !Array.isArray(document)) {
            cliError = document;
        }
    }
    catch {
        // Non-JSON diagnostics from older binaries remain available as stderr.
    }
    const details = { argv: [executable, ...args], exitCode: result.exitCode, stderr: result.stderr, ...(cliError === undefined ? {} : { cliError }) };
    if (isTerminalVerb(verb)) {
        const found = refused(verb, cliMessage(cliError) ?? result.stderr);
        if (found !== undefined)
            return refusal(verb, found, undefined, details);
    }
    return new PhuxError("command_failed", `phux ${verb} failed with exit code ${String(result.exitCode)}${diagnosticSuffix(result.stderr)}`, details);
}
export function diagnosticSuffix(stderr) {
    const detail = stderr.trim();
    return detail.length === 0 ? "" : `: ${detail}`;
}
/** What `phux snapshot|wait --json` prints for a tree or a typed document instead of a screen. */
function documentKind(value) {
    if (value === null || typeof value !== "object" || Array.isArray(value) || "cols" in value || !("seq" in value))
        return undefined;
    if ("document" in value)
        return "accessibility_tree";
    return "content_type" in value ? "document" : undefined;
}
/** `parseScreenState`, refusing the document a non-Terminal target printed instead. */
export function screenParser(verb, target) {
    return (value) => {
        try {
            return parseScreenState(value);
        }
        catch (cause) {
            const kind = documentKind(value);
            if (kind === undefined)
                throw cause;
            throw refusal(verb, { id: undefined, kind }, target);
        }
    };
}
