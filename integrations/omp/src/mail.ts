import type { ExtensionAPI } from "@oh-my-pi/pi-coding-agent";

import { MailInbox, releaseMail, waitForMail, type MailDelivery } from "../../runtime/src/mail.js";

/** Custom message type of mail from other agents in an OMP session. */
export const PHUX_MAIL_CUSTOM_TYPE = "phux.mail";

export interface OmpMailOptions {
  readonly env: NodeJS.ProcessEnv;
  readonly executable: string;
  /** Takes the next mail; tests replace the `phux mail wait` subprocess. */
  readonly wait?: (signal: AbortSignal) => Promise<MailDelivery | null>;
  /** Returns mail OMP refused; tests replace the `phux mail release` subprocess. */
  readonly release?: (delivery: MailDelivery) => Promise<void>;
  readonly onError?: (error: unknown) => void;
}

/**
 * Put this pane's mail in the OMP conversation with `pi.sendMessage`: a
 * follow-up while the agent works, a new turn while it is idle. Off outside
 * a phux pane, where there is no address to receive mail.
 */
export function registerOmpMail(pi: ExtensionAPI, options: OmpMailOptions): MailInbox | null {
  const inbox = addressed(options.env) ? conversationInbox(pi, options) : null;
  pi.on("session_start", () => inbox?.start());
  // Re-arms a loop that stopped on repeated failures, at the next finished turn.
  pi.on("agent_end", () => inbox?.start());
  pi.on("session_shutdown", () => inbox?.stop());
  return inbox;
}

function conversationInbox(pi: ExtensionAPI, options: OmpMailOptions): MailInbox {
  return new MailInbox({
    wait: options.wait ?? ((signal) => waitForMail({ executable: options.executable, env: options.env }, signal)),
    release: options.release ??
      ((delivery) => releaseMail({ executable: options.executable, env: options.env }, delivery.ids)),
    deliver: (delivery) => {
      pi.sendMessage(
        { customType: PHUX_MAIL_CUSTOM_TYPE, content: delivery.text, display: true, details: { ids: delivery.ids } },
        { triggerTurn: true, deliverAs: "followUp" },
      );
    },
    ...(options.onError === undefined ? {} : { onError: options.onError }),
  });
}

function addressed(env: NodeJS.ProcessEnv): boolean {
  return [env.PHUX_TERMINAL_ID, env.PHUX_AGENT_ADDRESS].some((value) => value !== undefined && value.trim() !== "");
}
