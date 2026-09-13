import { PhuxError } from "./errors.js";
import { DEFAULT_MAX_OUTPUT_BYTES, nodeProcessRunner, } from "./runner.js";
import { AGENT_EVENT_TYPES, isAgentEventType, parseAgentEmitResult, parseAgentRecord, parseAgentSessionOpenResult, parseAgentStateList, parseAskedEvent, parseCreateResult, parseInsertPaneResult, parseLaunchResult, parseMovePaneResult, parseRenderedFrame, parseRunResult, parseScreenState, parseSessionList, parseSpawnResult, parseSwapPaneResult, parseWatchEvent, SchemaValidationError, } from "./schemas.js";
export { AGENT_EVENT_TYPES, isAgentEventType } from "./schemas.js";
export const MINIMUM_PHUX_VERSION = "0.1.0";
const VERSION_PATTERN = /^phux\s+v?(\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?)$/;
export class PhuxCli {
    executable;
    socket;
    cwd;
    env;
    runner;
    maxStdoutBytes;
    maxStderrBytes;
    constructor(options = {}) {
        this.executable = options.executable ?? "phux";
        this.socket = options.socket;
        this.cwd = options.cwd;
        this.env = options.env;
        this.runner = options.runner ?? nodeProcessRunner;
        this.maxStdoutBytes = options.maxStdoutBytes ?? DEFAULT_MAX_OUTPUT_BYTES;
        this.maxStderrBytes = options.maxStderrBytes ?? DEFAULT_MAX_OUTPUT_BYTES;
        requireNonNegativeInteger(this.maxStdoutBytes, "maxStdoutBytes");
        requireNonNegativeInteger(this.maxStderrBytes, "maxStderrBytes");
    }
    async probe(options = {}) {
        try {
            const result = await this.execute(["--version"], options);
            if (result.termination !== "completed")
                this.throwTermination(result, [this.executable, "--version"]);
            if (result.exitCode !== 0) {
                return { available: false, reason: failureMessage(result) };
            }
            const rawVersion = result.stdout.trim();
            const match = VERSION_PATTERN.exec(rawVersion);
            if (match === null || match[1] === undefined) {
                return {
                    available: false,
                    rawVersion,
                    reason: `unexpected version output; expected "phux X.Y.Z", got ${JSON.stringify(rawVersion)}`,
                };
            }
            const version = match[1];
            if (!isCompatiblePhuxVersion(version)) {
                return {
                    available: false,
                    version,
                    rawVersion,
                    reason: `@phux/pi requires phux >= ${MINIMUM_PHUX_VERSION}; found ${version}`,
                };
            }
            return { available: true, version, rawVersion };
        }
        catch (error) {
            if (error instanceof PhuxError &&
                (error.code === "aborted" || error.code === "timeout" || error.code === "output_limit")) {
                throw error;
            }
            return {
                available: false,
                reason: isMissingExecutable(error)
                    ? `phux executable ${JSON.stringify(this.executable)} was not found; install phux or configure its absolute path`
                    : error instanceof Error ? error.message : String(error),
            };
        }
    }
    async ls(options = {}) {
        const args = this.withSocket(["ls", "--json"]);
        return this.jsonCommand("ls", args, options, parseSessionList);
    }
    /** Inventory panes and their owning session through the documented agent CLI projection. */
    async agentList(options = {}) {
        const args = this.withSocket(["agent", "list", "--json"]);
        return this.jsonCommand("agent list", args, options, parseAgentStateList);
    }
    async create(name, options = {}) {
        if (name.trim().length === 0)
            throw new TypeError("name must be non-empty");
        const args = ["new", "--json", "-s", name];
        if (options.cwd !== undefined)
            args.push("--cwd", options.cwd);
        this.pushSocket(args);
        if (options.command !== undefined) {
            if (options.command.length === 0)
                throw new TypeError("command must contain at least one argv item");
            args.push("--", ...options.command);
        }
        return this.jsonCommand("new", args, options, parseCreateResult);
    }
    async spawn(options = {}) {
        validatePlacement(options, true);
        const args = ["spawn", "--json"];
        if (options.satellite !== undefined)
            args.push("--satellite", options.satellite);
        if (options.target !== undefined)
            args.push("--target", options.target);
        if (options.split !== undefined)
            args.push("--split", options.split);
        if (options.ratio !== undefined)
            args.push("--ratio", String(options.ratio));
        if (options.cwd !== undefined)
            args.push("--cwd", options.cwd);
        this.pushSocket(args);
        if (options.command !== undefined) {
            if (options.command.length === 0)
                throw new TypeError("command must contain at least one argv item");
            args.push("--", ...options.command);
        }
        return this.jsonCommand("spawn", args, options, parseSpawnResult);
    }
    async launch(integration, options = {}) {
        if (integration.trim().length === 0)
            throw new TypeError("integration must be non-empty");
        validatePlacement(options, false);
        const args = ["launch", "--json"];
        if (options.target !== undefined)
            args.push("--target", options.target);
        if (options.split !== undefined)
            args.push("--split", options.split);
        if (options.ratio !== undefined)
            args.push("--ratio", String(options.ratio));
        if (options.cwd !== undefined)
            args.push("--cwd", options.cwd);
        this.pushSocket(args);
        args.push(integration);
        if (options.extra !== undefined) {
            if (options.extra.length === 0)
                throw new TypeError("extra must contain at least one argv item");
            args.push("--", ...options.extra);
        }
        return this.jsonCommand("launch", args, options, parseLaunchResult);
    }
    async insertPane(target, newPane, options = {}) {
        validateSpatial(target, newPane, options);
        const args = ["insert-pane", "--json"];
        pushSpatialGeometry(args, options);
        this.pushSocket(args);
        args.push(target, newPane);
        return this.jsonCommand("insert-pane", args, options, parseInsertPaneResult);
    }
    async movePane(source, target, options = {}) {
        validateSpatial(source, target, options);
        const args = ["move-pane", "--json"];
        pushSpatialGeometry(args, options);
        this.pushSocket(args);
        args.push(source, target);
        return this.jsonCommand("move-pane", args, options, parseMovePaneResult);
    }
    async swapPane(first, second, options = {}) {
        validateDistinctTargets(first, second);
        const args = ["swap-pane", "--json"];
        this.pushSocket(args);
        args.push(first, second);
        return this.jsonCommand("swap-pane", args, options, parseSwapPaneResult);
    }
    /** Read one pane's public projection, including declared-record provenance. */
    async agentShow(options) {
        const args = ["agent", "show", "--json"];
        this.pushSocket(args);
        args.push(options.target);
        return this.jsonCommand("agent show", args, options, parseAgentStateList);
    }
    /** Write and parse the CLI's confirmed whole-record response. */
    async agentSet(target, record, options = {}) {
        // `--state` / `--attention` are omitted when the record does not declare
        // them: passing a state marks the pane `declared` and stands the server's
        // detector down for the record's lifetime (L3.md 3.7, ADR-0046 point 8).
        const args = [
            "agent", "set", target,
            "--name", record.name,
            "--kind", record.kind,
            ...(record.state === undefined ? [] : ["--state", record.state]),
            ...(record.attention === undefined ? [] : ["--attention", record.attention]),
            "--session", record.session,
        ];
        this.pushSocket(args);
        const result = await this.completed("agent set", args, options, false);
        return parseAgentConfirmation("agent set", this.executable, result.stdout, args);
    }
    /** Clear a declaration and require the CLI's confirmed tombstone response. */
    async agentClear(target, options = {}) {
        const args = ["agent", "clear", target];
        this.pushSocket(args);
        const result = await this.completed("agent clear", args, options, false);
        if (!/^@\d+\t-$/.test(result.stdout.trim())) {
            throw invalidResponse("agent clear", this.executable, args, "expected @N\\t- confirmation");
        }
    }
    /** Open an AgentSession bound to a pane; the caller becomes its producer. */
    async agentSessionOpen(target, options) {
        if (target.trim().length === 0)
            throw new TypeError("target must be non-empty");
        if (options.provider.trim().length === 0)
            throw new TypeError("provider must be non-empty");
        const args = ["agent", "session", "open", target, "--provider", options.provider];
        if (options.nativeId !== undefined) {
            if (options.nativeId.trim().length === 0)
                throw new TypeError("nativeId must be non-empty");
            args.push("--native-id", options.nativeId);
        }
        args.push("--json");
        this.pushSocket(args);
        return this.jsonCommand("agent session open", args, options, parseAgentSessionOpenResult);
    }
    /** Append one closed-type record. No-op-refused by the server if this client is not the opener. */
    async agentEmit(target, type, options = {}) {
        if (target.trim().length === 0)
            throw new TypeError("target must be non-empty");
        if (!isAgentEventType(type)) {
            throw new TypeError(`type must be one of ${AGENT_EVENT_TYPES.join(", ")}`);
        }
        const args = ["agent", "emit", target, "--type", type];
        if (options.data !== undefined) {
            if (typeof options.data !== "object" || Array.isArray(options.data)) {
                throw new TypeError("data must be a JSON object");
            }
            args.push("--data", JSON.stringify(options.data));
        }
        args.push("--json");
        this.pushSocket(args);
        return this.jsonCommand("agent emit", args, options, parseAgentEmitResult);
    }
    /** Close a pane's AgentSession; the parent pane is untouched. */
    async agentSessionClose(target, options = {}) {
        if (target.trim().length === 0)
            throw new TypeError("target must be non-empty");
        const args = ["agent", "session", "close", target];
        this.pushSocket(args);
        const result = await this.completed("agent session close", args, options, false);
        return parseSessionClosed("agent session close", this.executable, result.stdout, args);
    }
    async renderedSnapshot(options) {
        requirePositiveInteger(options.cols, "cols");
        requirePositiveInteger(options.rows, "rows");
        const args = ["snapshot", "--rendered", "--json", "--cols", String(options.cols), "--rows", String(options.rows)];
        this.pushSocket(args);
        if (options.session !== undefined)
            args.push(options.session);
        return this.jsonCommand("snapshot --rendered", args, options, parseRenderedFrame);
    }
    async snapshot(options = {}) {
        const args = ["snapshot", "--json"];
        if (options.scrollback === true)
            args.push("--scrollback");
        else if (typeof options.scrollback === "number") {
            requireNonNegativeInteger(options.scrollback, "scrollback");
            args.push("--scrollback", String(options.scrollback));
        }
        if (options.cells === true)
            args.push("--cells");
        this.pushSocket(args);
        if (options.target !== undefined)
            args.push(options.target);
        return this.jsonCommand("snapshot", args, options, parseScreenState);
    }
    async wait(options = {}) {
        const args = ["wait", "--json"];
        if (options.until !== undefined)
            args.push("--until", options.until);
        if (options.idleMs !== undefined) {
            requireNonNegativeInteger(options.idleMs, "idleMs");
            args.push("--idle", String(options.idleMs));
        }
        if (options.phuxTimeoutSeconds !== undefined) {
            requireNonNegativeInteger(options.phuxTimeoutSeconds, "phuxTimeoutSeconds");
            args.push("--timeout", String(options.phuxTimeoutSeconds));
        }
        this.pushSocket(args);
        if (options.target !== undefined)
            args.push(options.target);
        const result = await this.completed("wait", args, options, true);
        if (result.exitCode !== 0 && result.exitCode !== 124) {
            throw commandFailed("wait", this.executable, args, result);
        }
        const screen = parseJson("wait", this.executable, result.stdout, args, parseScreenState);
        return { outcome: result.exitCode === 124 ? "timed_out" : "satisfied", screen };
    }
    async run(target, command, options = {}) {
        if (command.length === 0)
            throw new TypeError("command must contain at least one argv item");
        const args = ["run", "--json"];
        if (options.phuxTimeoutSeconds !== undefined) {
            requireNonNegativeInteger(options.phuxTimeoutSeconds, "phuxTimeoutSeconds");
            args.push("--timeout", String(options.phuxTimeoutSeconds));
        }
        this.pushSocket(args);
        args.push(target, ...command);
        const result = await this.completed("run", args, options, true);
        if (result.exitCode !== 0 && result.stdout.trim().length === 0) {
            throw commandFailed("run", this.executable, args, result);
        }
        const parsed = parseJson("run", this.executable, result.stdout, args, parseRunResult);
        const expectedExit = parsed.exit_code >= 0 && parsed.exit_code <= 255 ? parsed.exit_code : 255;
        if (result.exitCode !== expectedExit) {
            throw invalidResponse("run", this.executable, args, `$.exit_code (${parsed.exit_code}) does not match process exit ${String(result.exitCode)}`);
        }
        return parsed;
    }
    async sendKeys(target, keys, options = {}) {
        if (keys.length === 0)
            throw new TypeError("keys must contain at least one item");
        const args = ["send-keys"];
        this.pushSocket(args);
        args.push(target, ...keys);
        await this.completed("send-keys", args, options, false);
    }
    async kill(target, options = {}) {
        const args = ["kill", target];
        this.pushSocket(args);
        await this.completed("kill", args, options, false);
    }
    async signal(target, signal, options = {}) {
        const args = ["signal", target, signal];
        this.pushSocket(args);
        await this.completed("signal", args, options, false);
    }
    async tag(action, target, tags = [], options = {}) {
        if (action !== "ls" && tags.length === 0)
            throw new TypeError("tags must contain at least one item");
        if (action === "ls" && tags.length !== 0)
            throw new TypeError("tag ls does not accept tags");
        const args = ["tag", action, target, ...tags];
        this.pushSocket(args);
        const result = await this.completed(`tag ${action}`, args, options, false);
        return parseTagRows(result.stdout, `tag ${action}`, this.executable, args);
    }
    async ask(target, question, options = {}) {
        if (question.trim().length === 0)
            throw new TypeError("question must be non-empty");
        const args = ["ask", target, "--json"];
        if (options.id !== undefined)
            args.push("--id", options.id);
        for (const suggestion of options.suggestions ?? [])
            args.push("--suggest", suggestion);
        if (options.elapsedSeconds !== undefined) {
            requireNonNegativeInteger(options.elapsedSeconds, "elapsedSeconds");
            args.push("--elapsed-seconds", String(options.elapsedSeconds));
        }
        this.pushSocket(args);
        args.push(question);
        return this.jsonCommand("ask", args, options, parseAskedEvent);
    }
    async watch(options) {
        requirePositiveInteger(options.durationMs, "durationMs");
        requirePositiveInteger(options.maxEvents, "maxEvents");
        const args = ["watch", "--json"];
        this.pushSocket(args);
        args.push(options.target);
        let result;
        try {
            result = await this.execute(args, { ...options, timeoutMs: options.durationMs });
        }
        catch (cause) {
            throw new PhuxError("unavailable", `could not start phux executable ${JSON.stringify(this.executable)}: ${errorText(cause)}`, {
                argv: [this.executable, ...args], cause,
            });
        }
        if (result.termination === "aborted")
            this.throwTermination(result, [this.executable, ...args]);
        if (result.termination === "output_limit")
            this.throwTermination(result, [this.executable, ...args]);
        if (result.termination === "completed" && result.exitCode !== 0) {
            throw commandFailed("watch", this.executable, args, result);
        }
        const events = parseWatchLines(result.stdout, this.executable, args);
        const truncated = events.length > options.maxEvents;
        return {
            events: truncated ? events.slice(-options.maxEvents) : events,
            truncated,
            ended: result.termination === "completed",
        };
    }
    async jsonCommand(verb, args, options, parser) {
        const result = await this.completed(verb, args, options, false);
        return parseJson(verb, this.executable, result.stdout, args, parser);
    }
    async completed(verb, args, options, allowNonzero) {
        let result;
        try {
            result = await this.execute(args, options);
        }
        catch (cause) {
            const message = isMissingExecutable(cause)
                ? `phux executable ${JSON.stringify(this.executable)} was not found; install phux or configure its absolute path`
                : `could not start phux executable ${JSON.stringify(this.executable)}: ${errorText(cause)}`;
            throw new PhuxError("unavailable", message, {
                argv: [this.executable, ...args],
                cause,
            });
        }
        this.throwTermination(result, [this.executable, ...args]);
        if (!allowNonzero && result.exitCode !== 0) {
            throw commandFailed(verb, this.executable, args, result);
        }
        return result;
    }
    execute(args, options) {
        const request = {
            executable: this.executable,
            args,
            ...(this.cwd === undefined ? {} : { cwd: this.cwd }),
            ...(this.env === undefined ? {} : { env: this.env }),
            ...(options.signal === undefined ? {} : { signal: options.signal }),
            ...(options.timeoutMs === undefined ? {} : { timeoutMs: options.timeoutMs }),
            maxStdoutBytes: this.maxStdoutBytes,
            maxStderrBytes: this.maxStderrBytes,
        };
        return this.runner(request);
    }
    throwTermination(result, argv) {
        if (result.termination === "aborted") {
            throw new PhuxError("aborted", "phux command was aborted", { argv, stderr: result.stderr });
        }
        if (result.termination === "timed_out") {
            throw new PhuxError("timeout", "phux command exceeded its local subprocess timeout", {
                argv,
                stderr: result.stderr,
            });
        }
        if (result.termination === "output_limit") {
            const limit = result.outputLimit === "stdout" ? this.maxStdoutBytes : this.maxStderrBytes;
            throw new PhuxError("output_limit", `phux command exceeded the ${String(limit)}-byte ${result.outputLimit} capture limit`, { argv, stderr: result.stderr });
        }
    }
    withSocket(args) {
        this.pushSocket(args);
        return args;
    }
    pushSocket(args) {
        if (this.socket !== undefined)
            args.push("--socket", this.socket);
    }
}
function parseJson(verb, executable, stdout, args, parser) {
    let value;
    try {
        value = JSON.parse(stdout);
    }
    catch (cause) {
        throw new PhuxError("malformed_json", `phux ${verb} returned malformed JSON: ${errorText(cause)}`, {
            argv: [executable, ...args],
            cause,
        });
    }
    try {
        return parser(value);
    }
    catch (cause) {
        if (cause instanceof SchemaValidationError) {
            throw invalidResponse(verb, executable, args, cause.message, cause);
        }
        throw cause;
    }
}
function parseAgentConfirmation(verb, executable, stdout, args) {
    const line = stdout.trim();
    const tab = line.indexOf("\t");
    if (tab < 2 || !/^@\d+$/.test(line.slice(0, tab))) {
        throw invalidResponse(verb, executable, args, "expected @N\\t<record-json> confirmation");
    }
    return parseJson(verb, executable, line.slice(tab + 1), args, parseAgentRecord);
}
function parseSessionClosed(verb, executable, stdout, args) {
    const line = stdout.trim();
    const tab = line.indexOf("\t");
    const resource = tab < 0 ? "" : line.slice(0, tab);
    if (resource.length === 0 || line.slice(tab + 1) !== "closed") {
        throw invalidResponse(verb, executable, args, "expected @N\\tclosed confirmation");
    }
    return { resource, closed: true };
}
function parseWatchLines(stdout, executable, args) {
    const lines = stdout.split("\n").filter((line) => line.trim().length > 0);
    return lines.map((line, index) => {
        let value;
        try {
            value = JSON.parse(line);
        }
        catch (cause) {
            throw new PhuxError("malformed_json", `phux watch returned malformed JSON on line ${String(index + 1)}: ${errorText(cause)}`, {
                argv: [executable, ...args], cause,
            });
        }
        try {
            return parseWatchEvent(value, `$[${index}] (phux watch --json line)`);
        }
        catch (cause) {
            if (cause instanceof SchemaValidationError) {
                throw invalidResponse("watch", executable, args, cause.message, cause);
            }
            throw cause;
        }
    });
}
function parseTagRows(stdout, verb, executable, args) {
    const lines = stdout.trim().length === 0 ? [] : stdout.trim().split("\n");
    if (lines.length === 0)
        throw invalidResponse(verb, executable, args, "expected at least one @N\\t<tag text> confirmation");
    return lines.map((line) => {
        const tab = line.indexOf("\t");
        const terminal = tab < 0 ? "" : line.slice(0, tab);
        if (!/^@\d+$/.test(terminal)) {
            throw invalidResponse(verb, executable, args, "expected @N\\t<tag text> confirmation");
        }
        return { terminal, tagsText: line.slice(tab + 1) };
    });
}
function invalidResponse(verb, executable, args, detail, cause) {
    return new PhuxError("invalid_response", `phux ${verb} JSON does not match its documented CLI shape: ${detail}`, { argv: [executable, ...args], cause });
}
function commandFailed(verb, executable, args, result) {
    return new PhuxError("command_failed", `phux ${verb} failed with exit code ${String(result.exitCode)}${diagnosticSuffix(result.stderr)}`, {
        argv: [executable, ...args],
        exitCode: result.exitCode,
        stderr: result.stderr,
    });
}
function diagnosticSuffix(stderr) {
    const detail = stderr.trim();
    return detail.length === 0 ? "" : `: ${detail}`;
}
function failureMessage(result) {
    return `phux --version exited ${String(result.exitCode)}${diagnosticSuffix(result.stderr)}`;
}
function isCompatiblePhuxVersion(version) {
    const core = version.split(/[+-]/, 1)[0]?.split(".").map(Number);
    if (core === undefined || core.length !== 3)
        return false;
    const [major = 0, minor = 0, patch = 0] = core;
    if (major !== 0)
        return major > 0;
    if (minor !== 1)
        return minor > 1;
    if (patch !== 0)
        return patch > 0;
    return !version.includes("-");
}
function isMissingExecutable(error) {
    return error instanceof Error && "code" in error && error.code === "ENOENT";
}
function errorText(error) {
    return error instanceof Error ? error.message : String(error);
}
const SATELLITE_PANE_SELECTOR = /^[^/\s]+\/@\d+$/;
function validatePlacement(options, allowSatellite) {
    if (options.target === undefined && (options.split !== undefined || options.ratio !== undefined)) {
        throw new TypeError("target is required when split or ratio is provided");
    }
    if (options.target !== undefined) {
        if (options.target.trim().length === 0)
            throw new TypeError("target must be non-empty");
        if (SATELLITE_PANE_SELECTOR.test(options.target)) {
            throw new TypeError("explicit placement is local-only; satellite pane targets are unsupported");
        }
    }
    if (options.satellite !== undefined) {
        if (!allowSatellite)
            throw new TypeError("satellite is not supported for launch placement");
        if (options.target !== undefined)
            throw new TypeError("satellite and target placement cannot be combined");
    }
    if (options.split !== undefined && options.split !== "horizontal" && options.split !== "vertical") {
        throw new TypeError("split must be horizontal or vertical");
    }
    if (options.ratio !== undefined)
        requireRatio(options.ratio);
}
function validateSpatial(first, second, options) {
    validateDistinctTargets(first, second);
    if (options.direction !== undefined && options.direction !== "horizontal" && options.direction !== "vertical") {
        throw new TypeError("direction must be horizontal or vertical");
    }
    if (options.ratio !== undefined)
        requireRatio(options.ratio);
}
function validateDistinctTargets(first, second) {
    if (first.trim().length === 0 || second.trim().length === 0) {
        throw new TypeError("spatial targets must be non-empty");
    }
    if (first === second)
        throw new TypeError("spatial actions require two distinct targets");
    if (SATELLITE_PANE_SELECTOR.test(first) || SATELLITE_PANE_SELECTOR.test(second)) {
        throw new TypeError("spatial actions require local pane targets");
    }
}
function pushSpatialGeometry(args, options) {
    if (options.direction !== undefined)
        args.push("--split", options.direction);
    if (options.ratio !== undefined)
        args.push("--ratio", String(options.ratio));
}
function requireRatio(value) {
    if (!Number.isFinite(value) || value <= 0 || value >= 1) {
        throw new RangeError("ratio must be finite and strictly between 0 and 1");
    }
}
function requireNonNegativeInteger(value, name) {
    if (!Number.isSafeInteger(value) || value < 0) {
        throw new RangeError(`${name} must be a non-negative safe integer`);
    }
}
function requirePositiveInteger(value, name) {
    if (!Number.isSafeInteger(value) || value < 1) {
        throw new RangeError(`${name} must be a positive safe integer`);
    }
}
export function hasAgentSessionCli(cli) {
    const candidate = cli;
    return typeof candidate.agentSessionOpen === "function" &&
        typeof candidate.agentEmit === "function" &&
        typeof candidate.agentSessionClose === "function";
}
/**
 * True when `phux agent session open` is missing: an older binary without the
 * verb, or a server that refuses with `unsupported_server`. Emit must then
 * fail closed; identity-only `agent set` still runs.
 */
export function isAgentSessionUnsupported(error) {
    if (!(error instanceof PhuxError) || error.code !== "command_failed")
        return false;
    const haystack = `${error.message}\n${error.stderr ?? ""}`;
    return /unsupported_server|unrecognized subcommand|invalid subcommand|unknown command|not a valid command/i
        .test(haystack);
}
/**
 * Open once per pane, emit only after a successful open, close the session we
 * opened. A missing verb marks the emitter unavailable for the rest of its
 * life so later hooks do not retry a command the server does not have.
 */
export class AgentSessionEmitter {
    cli;
    provider;
    onError;
    target = null;
    nativeId = null;
    opened = false;
    unavailable = false;
    constructor(cli, options) {
        this.cli = cli !== null && hasAgentSessionCli(cli) ? cli : null;
        this.provider = options.provider;
        this.onError = options.onError ?? (() => { });
        if (this.cli === null)
            this.unavailable = true;
        if (this.provider.trim().length === 0)
            throw new TypeError("provider must be non-empty");
    }
    get isOpen() {
        return this.opened;
    }
    get isUnavailable() {
        return this.unavailable;
    }
    /** Take over a session this process left open (extension reload). */
    adopt(target) {
        this.target = target;
        this.opened = target !== null;
        this.nativeId = null;
    }
    async bind(target, nativeId, options = {}) {
        if (this.unavailable)
            return;
        if (target === this.target && this.opened && this.nativeId === nativeId)
            return;
        if (this.opened && (this.target !== target || this.nativeId !== nativeId)) {
            await this.finish(options);
        }
        if (target === null)
            return;
        await this.open(target, nativeId, options);
    }
    async emit(type, data, options = {}) {
        if (this.unavailable || !this.opened || this.target === null || this.cli === null)
            return;
        try {
            await this.cli.agentEmit(this.target, type, {
                ...options,
                ...(data === undefined ? {} : { data }),
            });
        }
        catch (error) {
            this.onError(error);
        }
    }
    async finish(options = {}) {
        if (this.unavailable || !this.opened || this.target === null || this.cli === null) {
            this.opened = false;
            this.target = null;
            this.nativeId = null;
            return;
        }
        const target = this.target;
        this.opened = false;
        this.target = null;
        this.nativeId = null;
        try {
            await this.cli.agentEmit(target, "session_end", options);
        }
        catch (error) {
            this.onError(error);
        }
        try {
            await this.cli.agentSessionClose(target, options);
        }
        catch (error) {
            this.onError(error);
        }
    }
    async open(target, nativeId, options) {
        if (this.cli === null)
            return;
        try {
            await this.cli.agentSessionOpen(target, {
                provider: this.provider,
                nativeId,
                ...options,
            });
        }
        catch (error) {
            if (isAgentSessionUnsupported(error)) {
                this.unavailable = true;
                return;
            }
            this.onError(error);
            return;
        }
        this.target = target;
        this.nativeId = nativeId;
        this.opened = true;
        try {
            await this.cli.agentEmit(target, "session_start", {
                ...options,
                data: { provider: this.provider, native_id: nativeId },
            });
        }
        catch (error) {
            this.onError(error);
        }
    }
}
