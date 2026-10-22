import { MailInbox, waitForMail, type MailDelivery } from "../../runtime/src/mail.js";

/** The one OpenCode session API mail needs: durable synthetic input. */
export interface SyntheticInput {
  synthetic(input: {
    readonly sessionID: string;
    readonly text: string;
    readonly description?: string;
    readonly metadata?: { readonly [key: string]: string[] };
    readonly delivery?: "steer" | "queue";
    readonly resume?: boolean;
  }): Promise<unknown>;
}

export interface OpenCodeMailOptions {
  readonly sessions: SyntheticInput;
  /** The session this TUI shows, which owns the pane and so its mail. */
  readonly visibleSession: () => string | undefined;
  readonly env: NodeJS.ProcessEnv;
  /** Takes the next mail; tests replace the `phux mail wait` subprocess. */
  readonly wait?: (signal: AbortSignal) => Promise<MailDelivery | null>;
  readonly onError?: (error: unknown) => void;
}

/**
 * This pane's mail, admitted to the visible session's inbox as a synthetic
 * message. `queue` waits for a running turn to end; `resume` starts one when
 * the session is idle. The TUI runs the loop only while a session is shown.
 */
export function openCodeMail(options: OpenCodeMailOptions): MailInbox {
  return new MailInbox({
    wait: options.wait ?? ((signal) => waitForMail({ env: options.env }, signal)),
    deliver: async (delivery) => {
      const sessionID = options.visibleSession();
      if (sessionID === undefined) {
        throw new Error(`no session is shown for mail ${delivery.ids.join(", ")}; it stays unread in \`phux mail read\``);
      }
      await options.sessions.synthetic({
        sessionID,
        text: delivery.text,
        description: "phux mail",
        metadata: { phux_mail_ids: [...delivery.ids] },
        delivery: "queue",
        resume: true,
      });
    },
    ...(options.onError === undefined ? {} : { onError: options.onError }),
  });
}
