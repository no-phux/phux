import { randomUUID } from "node:crypto";

import type { AuthEvent, AuthInteraction, AuthPrompt, AuthType, Models } from "@earendil-works/pi-ai";

/**
 * One provider login at a time, driven from wherever the human is. The
 * provider flow runs here on the host; each step it needs from the human
 * (open a URL, enter a device code, paste the redirect URL or code, pick an
 * option, type a key) is published as a `login` record, and the answer comes
 * back as a `login_answer` envelope. Nothing assumes a browser on this host:
 * the loopback callback some providers race against simply loses, and the
 * pasted code wins.
 */
export type LoginStep =
  | { readonly step: "started"; readonly method: AuthType }
  | { readonly step: "url"; readonly url: string; readonly instructions?: string }
  | { readonly step: "device_code"; readonly user_code: string; readonly verification_uri: string; readonly expires_in?: number }
  | { readonly step: "prompt"; readonly prompt: PublishedPrompt }
  | { readonly step: "info"; readonly message: string; readonly links?: readonly { url: string; label?: string }[] }
  | { readonly step: "progress"; readonly message: string }
  | { readonly step: "done" }
  | { readonly step: "failed"; readonly error: string }
  | { readonly step: "cancelled" };

export interface PublishedPrompt {
  readonly kind: AuthPrompt["type"];
  readonly message: string;
  readonly placeholder?: string;
  readonly options?: readonly { id: string; label: string; description?: string }[];
}

export interface LoginRecord {
  readonly login: string;
  readonly provider: string;
}

export type LoginPublisher = (login: LoginRecord, step: LoginStep) => void;

type LoginModels = Pick<Models, "login" | "getProvider">;

export class LoginBroker {
  private active: { record: LoginRecord; controller: AbortController; answer?: ((value: string) => void) | undefined } | undefined;

  constructor(private readonly models: LoginModels, private readonly publish: LoginPublisher) {}

  get current(): LoginRecord | undefined {
    return this.active?.record;
  }

  /** Start a login; a second start cancels the first. Resolves when the flow settles. */
  async start(provider: string, method: AuthType | undefined): Promise<void> {
    const auth = this.models.getProvider(provider)?.auth;
    if (auth === undefined) throw new Error(`unknown provider ${provider}`);
    const chosen: AuthType = method ?? (auth.oauth ? "oauth" : "api_key");
    if (chosen === "oauth" && !auth.oauth) throw new Error(`${provider} has no OAuth login; use api_key`);
    if (chosen === "api_key" && !auth.apiKey?.login) throw new Error(`${provider} has no interactive API-key login`);
    this.cancel();
    const record: LoginRecord = { login: randomUUID(), provider };
    const controller = new AbortController();
    const active: NonNullable<LoginBroker["active"]> = { record, controller };
    this.active = active;
    const interaction: AuthInteraction = {
      signal: controller.signal,
      notify: (event) => this.publish(record, fromEvent(event)),
      prompt: (prompt) => new Promise<string>((resolve, reject) => {
        const abort = () => reject(new Error("prompt cancelled"));
        prompt.signal?.addEventListener("abort", abort, { once: true });
        controller.signal.addEventListener("abort", abort, { once: true });
        active.answer = (value) => {
          active.answer = undefined;
          prompt.signal?.removeEventListener("abort", abort);
          resolve(value);
        };
        this.publish(record, { step: "prompt", prompt: publishedPrompt(prompt) });
      }),
    };
    this.publish(record, { step: "started", method: chosen });
    try {
      await this.models.login(provider, chosen, interaction);
      this.publish(record, { step: "done" });
    } catch (error) {
      this.publish(record, controller.signal.aborted
        ? { step: "cancelled" }
        : { step: "failed", error: error instanceof Error ? error.message : String(error) });
    } finally {
      if (this.active === active) this.active = undefined;
    }
  }

  /** Answer the open prompt of login `id`. False when nothing is waiting. */
  answer(id: string, value: string): boolean {
    const active = this.active;
    if (active === undefined || active.record.login !== id || active.answer === undefined) return false;
    active.answer(value);
    return true;
  }

  cancel(id?: string): boolean {
    const active = this.active;
    if (active === undefined || (id !== undefined && active.record.login !== id)) return false;
    active.controller.abort();
    return true;
  }
}

function fromEvent(event: AuthEvent): LoginStep {
  switch (event.type) {
    case "auth_url":
      return { step: "url", url: event.url, ...(event.instructions === undefined ? {} : { instructions: event.instructions }) };
    case "device_code":
      return {
        step: "device_code",
        user_code: event.userCode,
        verification_uri: event.verificationUri,
        ...(event.expiresInSeconds === undefined ? {} : { expires_in: event.expiresInSeconds }),
      };
    case "info":
      return { step: "info", message: event.message, ...(event.links === undefined ? {} : { links: event.links }) };
    case "progress":
      return { step: "progress", message: event.message };
  }
}

function publishedPrompt(prompt: AuthPrompt): PublishedPrompt {
  return {
    kind: prompt.type,
    message: prompt.message,
    ...("placeholder" in prompt && prompt.placeholder !== undefined ? { placeholder: prompt.placeholder } : {}),
    ...(prompt.type === "select" ? { options: prompt.options } : {}),
  };
}
