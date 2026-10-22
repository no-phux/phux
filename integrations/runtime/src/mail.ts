/**
 * Mail into an agent's conversation, through the harness's own API.
 *
 * `phux mail wait` long-polls this host's work store and takes each unread
 * message for this agent once. A harness integration runs a {@link MailInbox}
 * and hands each delivery to its own message API. Nothing here, or anywhere
 * in mail delivery, types into a terminal.
 */
import { PhuxError } from "./errors.js";
import { nodeProcessRunner, type ProcessResult, type ProcessRunner } from "./runner.js";

/** Taken mail, framed for the agent by `phux mail wait`. */
export interface MailDelivery {
  /** What the agent reads: sender, thread, body, and how to reply. */
  readonly text: string;
  /** The taken message ids, oldest first. */
  readonly ids: readonly string[];
}

export interface MailWaitOptions {
  readonly executable?: string;
  readonly env?: NodeJS.ProcessEnv;
  readonly runner?: ProcessRunner;
  /** How long one wait holds when there is no mail. */
  readonly timeoutSeconds?: number;
}

/** One wait's length: long enough that idle agents cost one process per five minutes. */
export const MAIL_WAIT_SECONDS = 300;
/** Slack past the keeper's own deadline before the subprocess counts as hung. */
const WAIT_SLACK_MS = 15_000;

/**
 * Run one `phux mail wait --json` and return what it took, or null when
 * its deadline passed with no mail. Aborting `signal` ends the wait; the
 * keeper sees the hang-up and takes nothing.
 */
export async function waitForMail(options: MailWaitOptions, signal: AbortSignal): Promise<MailDelivery | null> {
  const executable = options.executable ?? "phux";
  const seconds = options.timeoutSeconds ?? MAIL_WAIT_SECONDS;
  const args = ["mail", "wait", "--json", "--timeout", String(seconds)];
  const argv = [executable, ...args];
  const result = await (options.runner ?? nodeProcessRunner)({
    executable,
    args,
    ...(options.env === undefined ? {} : { env: options.env }),
    signal,
    timeoutMs: seconds * 1000 + WAIT_SLACK_MS,
  });
  if (result.termination !== "completed" || result.exitCode !== 0) throw waitFailed(result, argv);
  return parseMailWait(result.stdout, argv);
}

/** The delivery in a `phux mail wait --json` document, or null when it took nothing. */
export function parseMailWait(stdout: string, argv: readonly string[] = ["phux", "mail", "wait"]): MailDelivery | null {
  let document: unknown;
  try {
    document = JSON.parse(stdout);
  } catch (cause) {
    throw new PhuxError("malformed_json", "phux mail wait returned malformed JSON", { argv, cause });
  }
  const invalid = (detail: string) =>
    new PhuxError("invalid_response", `phux mail wait returned an invalid document: ${detail}`, { argv });
  if (document === null || typeof document !== "object") throw invalid("not an object");
  const { schema, mail, text } = document as Record<string, unknown>;
  if (schema !== "phux.coord/v1") throw invalid("schema is not phux.coord/v1");
  if (!Array.isArray(mail)) throw invalid("mail is not an array");
  const ids = mail.map((message: unknown) => {
    const id = (message as { readonly id?: unknown } | null)?.id;
    if (typeof id !== "string") throw invalid("a message has no id");
    return id;
  });
  if (ids.length === 0) return null;
  if (typeof text !== "string") throw invalid("text is missing beside taken mail");
  return { text, ids };
}

function waitFailed(result: ProcessResult, argv: readonly string[]): PhuxError {
  const stderr = result.stderr.trim().slice(0, 2048);
  if (result.termination === "aborted") return new PhuxError("aborted", "phux mail wait was aborted", { argv, stderr });
  if (result.termination === "timed_out") {
    return new PhuxError("timeout", "phux mail wait outlived its deadline", { argv, stderr });
  }
  if (result.termination === "output_limit") {
    return new PhuxError("output_limit", "phux mail wait exceeded its output limit", { argv, stderr });
  }
  return new PhuxError("command_failed", `phux mail wait failed${stderr === "" ? "" : `: ${stderr}`}`, {
    argv, exitCode: result.exitCode, stderr,
  });
}

export interface MailInboxOptions {
  /** Take the next mail, or null at the wait's deadline. */
  readonly wait: (signal: AbortSignal) => Promise<MailDelivery | null>;
  /** Put taken mail in the agent's conversation. */
  readonly deliver: (delivery: MailDelivery) => Promise<void> | void;
  readonly onError?: (error: unknown) => void;
}

/**
 * One agent's mail loop: wait, deliver, repeat. A failed wait is retried
 * once at once, since a keeper replaced by an upgrade closes its waiters;
 * a second consecutive failure stops the loop until the harness calls
 * {@link MailInbox.start} again on its next lifecycle event.
 */
export class MailInbox {
  private controller: AbortController | null = null;

  constructor(private readonly options: MailInboxOptions) {}

  get running(): boolean {
    return this.controller !== null;
  }

  /** Start waiting, unless already waiting. */
  start(): void {
    if (this.controller !== null) return;
    const controller = new AbortController();
    this.controller = controller;
    void this.run(controller).finally(() => {
      if (this.controller === controller) this.controller = null;
    });
  }

  /** Stop waiting. Mail not yet taken stays in the store. */
  stop(): void {
    this.controller?.abort();
    this.controller = null;
  }

  private async run(controller: AbortController): Promise<void> {
    let failedLast = false;
    while (!controller.signal.aborted) {
      try {
        const delivery = await this.options.wait(controller.signal);
        failedLast = false;
        if (delivery !== null) await this.options.deliver(delivery);
      } catch (error) {
        if (controller.signal.aborted) return;
        this.options.onError?.(error);
        if (failedLast) return;
        failedLast = true;
      }
    }
  }
}
