/**
 * What reaches the chief through its pane's input (docs/chief.md, "Phone to
 * chief"). A phone sends one line of JSON, `{"chief":1,...}`, so multi-line
 * text, ids, and the lock flag survive the terminal. A person typing in the
 * pane sends plain text or a slash command and is treated as unlocked: they
 * are at the host.
 */

export const ENVELOPE_VERSION = 1;
const MAX_TEXT = 64 * 1024;
const ID = /^[A-Za-z0-9._:-]{1,128}$/;

export type Command =
  | { readonly type: "message"; readonly id: string | undefined; readonly text: string; readonly locked: boolean }
  | { readonly type: "unlock"; readonly id: string | undefined }
  | { readonly type: "abort"; readonly id: string | undefined }
  | { readonly type: "status"; readonly id: string | undefined }
  | { readonly type: "models"; readonly id: string | undefined; readonly query: string | undefined }
  | { readonly type: "model"; readonly id: string | undefined; readonly model: string; readonly thinking: string | undefined }
  | { readonly type: "thinking"; readonly id: string | undefined; readonly level: string }
  | { readonly type: "login"; readonly id: string | undefined; readonly provider: string; readonly method: "oauth" | "api_key" | undefined }
  | { readonly type: "login_answer"; readonly id: string | undefined; readonly login: string; readonly value: string }
  | { readonly type: "login_cancel"; readonly id: string | undefined; readonly login: string }
  | { readonly type: "logout"; readonly id: string | undefined; readonly provider: string }
  | { readonly type: "compact"; readonly id: string | undefined; readonly instructions: string | undefined }
  | { readonly type: "help"; readonly id: string | undefined };

export type Parsed = { readonly ok: true; readonly command: Command } | { readonly ok: false; readonly id: string | undefined; readonly error: string };

/** One submitted unit of pane input: a typed line or a bracketed paste. */
export function parseInput(raw: string): Parsed | null {
  const text = raw.replace(/\r\n?/g, "\n");
  const trimmed = text.trim();
  if (trimmed.length === 0) return null;
  if (trimmed.startsWith("{") && trimmed.includes("\"chief\"")) return parseEnvelope(trimmed);
  if (trimmed.startsWith("/")) return parseSlash(trimmed);
  return { ok: true, command: { type: "message", id: undefined, text: trimmed, locked: false } };
}

function parseEnvelope(line: string): Parsed {
  let value: unknown;
  try {
    value = JSON.parse(line);
  } catch {
    return { ok: false, id: undefined, error: "envelope is not valid JSON" };
  }
  if (value === null || typeof value !== "object" || Array.isArray(value)) return { ok: false, id: undefined, error: "envelope must be an object" };
  const v = value as Record<string, unknown>;
  const id = typeof v.id === "string" && ID.test(v.id) ? v.id : undefined;
  const fail = (error: string): Parsed => ({ ok: false, id, error });
  if (v.chief !== ENVELOPE_VERSION) return fail(`unsupported envelope version ${JSON.stringify(v.chief)}; this chief speaks ${ENVELOPE_VERSION}`);
  if (v.id !== undefined && id === undefined) return fail("id must match [A-Za-z0-9._:-]{1,128}");
  const str = (key: string, required = true): string | undefined | Parsed => {
    const field = v[key];
    if (field === undefined && !required) return undefined;
    if (typeof field !== "string" || field.trim().length === 0) return fail(`${key} must be a non-empty string`);
    if (field.length > MAX_TEXT) return fail(`${key} exceeds ${MAX_TEXT} characters`);
    return field;
  };
  const ok = (command: Command): Parsed => ({ ok: true, command });
  switch (v.type) {
    case "message": {
      const text = str("text");
      if (typeof text !== "string") return text as Parsed;
      if (v.locked !== undefined && typeof v.locked !== "boolean") return fail("locked must be a boolean");
      return ok({ type: "message", id, text: text.trim(), locked: v.locked === true });
    }
    case "unlock":
    case "abort":
    case "status":
    case "help":
      return ok({ type: v.type, id });
    case "models": {
      const query = str("query", false);
      if (typeof query === "object") return query;
      return ok({ type: "models", id, query });
    }
    case "model": {
      const model = str("model");
      if (typeof model !== "string") return model as Parsed;
      const thinking = str("thinking", false);
      if (typeof thinking === "object") return thinking;
      return ok({ type: "model", id, model: model.trim(), thinking });
    }
    case "thinking": {
      const level = str("level");
      if (typeof level !== "string") return level as Parsed;
      return ok({ type: "thinking", id, level });
    }
    case "login": {
      const provider = str("provider");
      if (typeof provider !== "string") return provider as Parsed;
      if (v.method !== undefined && v.method !== "oauth" && v.method !== "api_key") return fail("method must be oauth or api_key");
      return ok({ type: "login", id, provider: provider.trim(), method: v.method as "oauth" | "api_key" | undefined });
    }
    case "login_answer": {
      const login = str("login");
      if (typeof login !== "string") return login as Parsed;
      if (typeof v.value !== "string") return fail("value must be a string");
      return ok({ type: "login_answer", id, login, value: v.value });
    }
    case "login_cancel": {
      const login = str("login");
      if (typeof login !== "string") return login as Parsed;
      return ok({ type: "login_cancel", id, login });
    }
    case "logout": {
      const provider = str("provider");
      if (typeof provider !== "string") return provider as Parsed;
      return ok({ type: "logout", id, provider: provider.trim() });
    }
    case "compact": {
      const instructions = str("instructions", false);
      if (typeof instructions === "object") return instructions;
      return ok({ type: "compact", id, instructions });
    }
    default:
      return fail(`unknown envelope type ${JSON.stringify(v.type)}`);
  }
}

function parseSlash(line: string): Parsed {
  const [head = "", ...rest] = line.slice(1).split(/\s+/);
  const arg = rest.join(" ").trim();
  const id = undefined;
  const need = (what: string): Parsed => ({ ok: false, id, error: `/${head} needs ${what}` });
  switch (head) {
    case "unlock":
    case "abort":
    case "status":
    case "help":
      return { ok: true, command: { type: head, id } };
    case "models":
      return { ok: true, command: { type: "models", id, query: arg || undefined } };
    case "model": {
      if (!arg) return need("provider/modelId");
      const [model = "", thinking] = arg.split(/\s+/);
      return { ok: true, command: { type: "model", id, model, thinking } };
    }
    case "thinking":
      return arg ? { ok: true, command: { type: "thinking", id, level: arg } } : need("a level");
    case "login": {
      if (!rest[0]) return need("a provider id");
      const method = rest[1];
      if (method !== undefined && method !== "oauth" && method !== "api_key") return { ok: false, id, error: "/login PROVIDER [oauth|api_key]" };
      return { ok: true, command: { type: "login", id, provider: rest[0], method } };
    }
    case "code": {
      // `/code LOGIN VALUE`: answer the open login prompt from the pane.
      if (!rest[0] || rest.length < 2) return need("LOGIN VALUE");
      return { ok: true, command: { type: "login_answer", id, login: rest[0], value: rest.slice(1).join(" ") } };
    }
    case "logout":
      return arg ? { ok: true, command: { type: "logout", id, provider: arg } } : need("a provider id");
    case "compact":
      return { ok: true, command: { type: "compact", id, instructions: arg || undefined } };
    default:
      return { ok: false, id, error: `unknown command /${head}; try /help` };
  }
}

export const HELP = [
  "Type a message, or:",
  "  /status                 model, lock, held messages, providers",
  "  /models [filter]        models with working credentials",
  "  /model provider/id [thinking]",
  "  /thinking off|minimal|low|medium|high|xhigh",
  "  /login provider [oauth|api_key]   then /code LOGIN VALUE when asked",
  "  /logout provider",
  "  /unlock                 release messages held while the phone was locked",
  "  /abort                  stop the current turn",
  "  /compact [instructions]",
].join("\n");
