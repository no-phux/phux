/** A stable, actionable error surface independent of the process host. */
export class PhuxError extends Error {
    code;
    argv;
    exitCode;
    stderr;
    constructor(code, message, details = {}) {
        super(message, details.cause === undefined ? undefined : { cause: details.cause });
        this.name = "PhuxError";
        this.code = code;
        this.argv = details.argv;
        this.exitCode = details.exitCode;
        this.stderr = details.stderr;
    }
}
