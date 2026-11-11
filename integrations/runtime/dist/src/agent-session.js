import { PhuxError } from "./errors.js";
export function hasAgentSessionCli(cli) {
    const candidate = cli;
    return typeof candidate.agentSessionOpen === "function" &&
        typeof candidate.agentEmit === "function" &&
        typeof candidate.agentSessionClose === "function";
}
/** Total time `agentEmit` spends retrying an upgrade-sealed stream. */
export const UPGRADE_RETRY_BUDGET_MS = 3000;
const UPGRADE_RETRY_BASE_MS = 50;
const UPGRADE_RETRY_MAX_DELAY_MS = 800;
/**
 * True for exactly the refusal a graceful upgrade gives an append between
 * its cut of the stream and the re-exec (L1 §4.8): `overflow` whose
 * diagnostic says the server is upgrading. Nothing was appended, so the
 * same record is safe to resend; every other `overflow` is not retried.
 */
export function isUpgradeSealRefusal(error) {
    if (!(error instanceof PhuxError) || error.code !== "command_failed")
        return false;
    const detail = error.cliError?.["error"];
    if (detail === null || typeof detail !== "object" || Array.isArray(detail))
        return false;
    const { code, message } = detail;
    return code === "overflow" && typeof message === "string" && /server is upgrading/.test(message);
}
/**
 * A graceful upgrade seals the stream from its cut until the re-exec and
 * refuses appends in that window; nothing was appended, so retry it.
 */
export async function retryUpgradeSeal(run, signal) {
    const started = Date.now();
    for (let attempt = 0;; attempt += 1) {
        try {
            return await run();
        }
        catch (error) {
            const delay = Math.min(UPGRADE_RETRY_BASE_MS * 2 ** attempt, UPGRADE_RETRY_MAX_DELAY_MS);
            if (!isUpgradeSealRefusal(error) || Date.now() - started + delay > UPGRADE_RETRY_BUDGET_MS) {
                throw error;
            }
            await sleep(delay, signal);
        }
    }
}
function sleep(ms, signal) {
    return new Promise((resolve, reject) => {
        if (signal?.aborted === true) {
            reject(abortedRetry());
            return;
        }
        const onAbort = () => {
            clearTimeout(timer);
            reject(abortedRetry());
        };
        const timer = setTimeout(() => {
            signal?.removeEventListener("abort", onAbort);
            resolve();
        }, ms);
        signal?.addEventListener("abort", onAbort, { once: true });
    });
}
function abortedRetry() {
    return new PhuxError("aborted", "phux agent emit was aborted while waiting out a server upgrade");
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
    resource = null;
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
        return this.resource !== null;
    }
    get isUnavailable() {
        return this.unavailable;
    }
    /** Adopt only an identity the host verified against its own declaration. */
    adopt(target, nativeId, identity) {
        if (identity.parent !== target || identity.provider !== this.provider || identity.native_id !== nativeId) {
            throw new Error("AgentSession identity does not match its hosting binding");
        }
        if (!/^(?:[^/\s]+\/)?@\d+$/.test(identity.resource) || identity.resource === target) {
            throw new Error("AgentSession identity must name its exact child resource");
        }
        this.target = target;
        this.nativeId = nativeId;
        this.resource = identity.resource;
    }
    async bind(target, nativeId, options = {}) {
        if (this.unavailable)
            return;
        if (target === this.target && this.isOpen && this.nativeId === nativeId)
            return;
        if (this.isOpen && (this.target !== target || this.nativeId !== nativeId)) {
            await this.finish(options);
        }
        if (target === null)
            return;
        await this.open(target, nativeId, options);
    }
    async emit(type, data, options = {}) {
        if (this.unavailable || this.resource === null || this.cli === null)
            return;
        try {
            await this.cli.agentEmit(this.resource, type, {
                ...options,
                ...(data === undefined ? {} : { data }),
            });
        }
        catch (error) {
            this.onError(error);
        }
    }
    async finish(options = {}) {
        const resource = this.resource;
        this.resource = null;
        this.target = null;
        this.nativeId = null;
        if (this.unavailable || resource === null || this.cli === null)
            return;
        try {
            await this.cli.agentEmit(resource, "session_end", options);
        }
        catch (error) {
            this.onError(error);
        }
        try {
            await this.cli.agentSessionClose(resource, options);
        }
        catch (error) {
            this.onError(error);
        }
    }
    async open(target, nativeId, options) {
        if (this.cli === null)
            return;
        try {
            const opened = await this.cli.agentSessionOpen(target, {
                provider: this.provider,
                nativeId,
                ...options,
            });
            this.adopt(target, nativeId, opened);
        }
        catch (error) {
            if (isAgentSessionUnsupported(error)) {
                this.unavailable = true;
                return;
            }
            this.onError(error);
            return;
        }
        await this.emit("session_start", { provider: this.provider, native_id: nativeId }, options);
    }
}
