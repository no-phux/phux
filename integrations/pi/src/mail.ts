import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

import { MailInbox, releaseMail, waitForMail, type MailDelivery } from "@phux/integration-runtime/mail";

/** Custom message type of mail from other agents in a Pi session. */
export const PHUX_MAIL_CUSTOM_TYPE = "phux.mail";

export interface PhuxMailOptions {
  readonly env: NodeJS.ProcessEnv;
  /** Takes the next mail; tests replace the `phux mail wait` subprocess. */
  readonly wait?: (signal: AbortSignal) => Promise<MailDelivery | null>;
  /** Returns mail Pi refused; tests replace the `phux mail release` subprocess. */
  readonly release?: (delivery: MailDelivery) => Promise<void>;
  readonly onError?: (error: unknown) => void;
}

/**
 * Put this pane's mail in the Pi conversation with `pi.sendMessage`: a
 * follow-up while the agent works, a new turn while it is idle. Off outside
 * a phux pane, where there is no address to receive mail.
 */
export function registerPhuxMail(pi: ExtensionAPI, options: PhuxMailOptions): MailInbox | null {
  const inbox = addressed(options.env) ? conversationInbox(pi, options) : null;
  pi.on("session_start", () => inbox?.start());
  // Re-arms a loop that stopped on repeated failures, at the next settled turn.
  pi.on("agent_settled", () => inbox?.start());
  pi.on("session_shutdown", () => inbox?.stop());
  return inbox;
}

function conversationInbox(pi: ExtensionAPI, options: PhuxMailOptions): MailInbox {
  return new MailInbox({
    // The runtime runs `$PHUX_BIN` from `env` when it is set, as OMP does.
    wait: options.wait ?? ((signal) => waitForMail({ env: options.env }, signal)),
    release: options.release ?? ((delivery) => releaseMail({ env: options.env }, delivery.ids)),
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
