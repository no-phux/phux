// Generated from phux own-code TypeScript by npm run generate. No provider SDK is bundled.
var __defProp = Object.defineProperty;
var __getOwnPropNames = Object.getOwnPropertyNames;
var __esm = (fn, res, err) => function __init() {
  if (err) throw err[0];
  try {
    return fn && (res = (0, fn[__getOwnPropNames(fn)[0]])(fn = 0)), res;
  } catch (e) {
    throw err = [e], e;
  }
};
var __export = (target, all) => {
  for (var name in all)
    __defProp(target, name, { get: all[name], enumerable: true });
};

// ../runtime/src/transcript.ts
function transcriptMode(value) {
  if (value === "0") return "off";
  return value === "full" ? "full" : "default";
}
function utf8Length(text) {
  return Buffer.byteLength(text, "utf8");
}
function keepHead(text, maxBytes) {
  if (utf8Length(text) <= maxBytes) return text;
  const { read } = encoder.encodeInto(text, new Uint8Array(Math.max(0, maxBytes)));
  return text.slice(0, read);
}
function keepTail(text, maxBytes) {
  let bytes = utf8Length(text);
  if (bytes <= maxBytes) return text;
  let tail = text.slice(-Math.max(0, maxBytes));
  bytes = utf8Length(tail);
  while (bytes > maxBytes) {
    tail = tail.slice(Math.max(1, Math.ceil((bytes - maxBytes) / 3)));
    bytes = utf8Length(tail);
  }
  const first = tail.charCodeAt(0);
  return first >= 56320 && first <= 57343 ? tail.slice(1) : tail;
}
function cleanText(text) {
  return text.replace(ESCAPES, "").replace(CONTROLS, "");
}
function oneLine(text, maxChars) {
  const collapsed = cleanText(text).split(/\s+/u).filter((word) => word.length > 0).join(" ");
  return Array.from(collapsed).slice(0, maxChars).join("");
}
function label(text) {
  return keepHead(oneLine(text, MAX_LABEL_BYTES), MAX_LABEL_BYTES);
}
function summarizeArgs(args) {
  if (args === null || typeof args !== "object" || Array.isArray(args)) {
    return typeof args === "string" ? oneLine(args, MAX_TOOL_SUMMARY_CHARS) : "";
  }
  const record3 = args;
  const named = SUMMARY_KEYS.map((key) => record3[key]).find((value) => typeof value === "string" && value.trim().length > 0);
  if (named !== void 0) return oneLine(named, MAX_TOOL_SUMMARY_CHARS);
  const pairs = Object.entries(record3).filter(([key]) => !CONTENT_KEYS.has(key)).map(([key, value]) => `${key}=${scalarSummary(value)}`);
  return oneLine(pairs.join(" "), MAX_TOOL_SUMMARY_CHARS);
}
function scalarSummary(value) {
  if (typeof value === "string") return oneLine(value, SUMMARY_VALUE_CHARS);
  if (typeof value === "number" || typeof value === "boolean" || value === null) return String(value);
  return "\u2026";
}
function transcriptData(provider2, entry) {
  const cleaned = cleanText(entry.text);
  let text = keepHead(cleaned, MAX_TRANSCRIPT_DATA_BYTES);
  let truncated = entry.truncated || text.length < cleaned.length;
  let tool = entry.role === "tool" && entry.tool !== void 0 ? {
    name: label(entry.tool.name),
    call_id: label(entry.tool.call_id),
    summary: oneLine(entry.tool.summary, MAX_TOOL_SUMMARY_CHARS),
    status: entry.tool.status,
    output: keepTail(cleanText(entry.tool.output), MAX_TOOL_OUTPUT_BYTES)
  } : void 0;
  const build = () => ({
    provider: label(provider2),
    schema: TRANSCRIPT_SCHEMA,
    entry: {
      id: label(entry.id),
      role: entry.role,
      text,
      truncated,
      final: entry.final,
      ...tool === void 0 ? {} : { tool }
    }
  });
  for (; ; ) {
    const data = build();
    const over = utf8Length(JSON.stringify(data)) - MAX_TRANSCRIPT_DATA_BYTES;
    if (over <= 0) return data;
    if (text.length > 0) {
      text = keepHead(text, utf8Length(text) - over);
      truncated = true;
    } else if (tool !== void 0 && tool.output.length > 0) {
      tool = { ...tool, output: keepTail(tool.output, utf8Length(tool.output) - over) };
    } else {
      return data;
    }
  }
}
var TRANSCRIPT_SCHEMA, MAX_RECORD_BYTES, ENVELOPE_RESERVE_BYTES, MAX_TRANSCRIPT_DATA_BYTES, MAX_TOOL_SUMMARY_CHARS, MAX_TOOL_OUTPUT_BYTES, MAX_LABEL_BYTES, PARTIAL_MIN_INTERVAL_MS, encoder, ESCAPES, CONTROLS, SUMMARY_KEYS, CONTENT_KEYS, SUMMARY_VALUE_CHARS, TranscriptGate;
var init_transcript = __esm({
  "../runtime/src/transcript.ts"() {
    "use strict";
    TRANSCRIPT_SCHEMA = "phux.transcript/v1";
    MAX_RECORD_BYTES = 16 * 1024;
    ENVELOPE_RESERVE_BYTES = 256;
    MAX_TRANSCRIPT_DATA_BYTES = MAX_RECORD_BYTES - ENVELOPE_RESERVE_BYTES;
    MAX_TOOL_SUMMARY_CHARS = 512;
    MAX_TOOL_OUTPUT_BYTES = 4 * 1024;
    MAX_LABEL_BYTES = 256;
    PARTIAL_MIN_INTERVAL_MS = 250;
    encoder = new TextEncoder();
    ESCAPES = /\u001b(?:\[[0-?]*[ -/]*[@-~]|\][^\u0007\u001b]*(?:\u0007|\u001b\\)?|[\s\S]?)/g;
    CONTROLS = /[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/g;
    SUMMARY_KEYS = [
      "command",
      "file_path",
      "notebook_path",
      "path",
      "pattern",
      "url",
      "query",
      "description",
      "prompt"
    ];
    CONTENT_KEYS = /* @__PURE__ */ new Set([
      "content",
      "contents",
      "new_string",
      "old_string",
      "newText",
      "oldText",
      "new_source",
      "edits",
      "text",
      "body",
      "data",
      "patch",
      "diff"
    ]);
    SUMMARY_VALUE_CHARS = 80;
    TranscriptGate = class {
      constructor(now = Date.now, intervalMs = PARTIAL_MIN_INTERVAL_MS, capacity = 256) {
        this.now = now;
        this.intervalMs = intervalMs;
        this.capacity = capacity;
      }
      now;
      intervalMs;
      capacity;
      sent = /* @__PURE__ */ new Map();
      admit(entry) {
        const previous = this.sent.get(entry.id);
        const at = this.now();
        if (entry.final) {
          if (previous?.final === true && previous.text === entry.text && previous.status === entry.tool?.status) {
            return false;
          }
        } else if (previous !== void 0) {
          if (previous.final) return false;
          if (entry.text.length <= previous.text.length) return false;
          if (at - previous.at < this.intervalMs) return false;
        }
        this.remember(entry, at);
        return true;
      }
      remember(entry, at) {
        this.sent.delete(entry.id);
        this.sent.set(entry.id, { text: entry.text, final: entry.final, status: entry.tool?.status, at });
        if (this.sent.size > this.capacity) {
          const oldest = this.sent.keys().next().value;
          if (oldest !== void 0) this.sent.delete(oldest);
        }
      }
    };
  }
});

// src/questions.ts
var QuestionRegistry;
var init_questions = __esm({
  "src/questions.ts"() {
    "use strict";
    QuestionRegistry = class {
      constructor(compile, publish, close) {
        this.compile = compile;
        this.publish = publish;
        this.close = close;
      }
      compile;
      publish;
      close;
      pending = /* @__PURE__ */ new Map();
      open(question, signal) {
        if (signal?.aborted) return Promise.reject(new Error("Provider question cancelled"));
        if (this.pending.get(question.id)?.settled === false) return Promise.reject(new Error("Duplicate active provider question ID"));
        const totalBytes = [...this.pending.values()].filter((item) => item.question.id !== question.id).reduce((sum, item) => sum + Buffer.byteLength(JSON.stringify(item.question)), 0) + Buffer.byteLength(JSON.stringify(question));
        if (!this.pending.has(question.id) && this.pending.size >= 32 || totalBytes > 12 * 1024 || question.id.length > 256) return Promise.reject(new Error("Native question bounds exceeded"));
        let validate;
        if (question.kind === "form" && question.fields !== null && typeof question.fields === "object" && !Array.isArray(question.fields) && "properties" in question.fields) {
          try {
            validate = this.compile(question.fields);
          } catch {
            return Promise.reject(new Error("Provider form schema is not supported by the native validator"));
          }
        }
        this.pending.get(question.id)?.dispose();
        return new Promise((resolve2, reject) => {
          const cancelled = () => {
            this.pending.delete(question.id);
            reject(new Error("Provider question cancelled"));
            this.close(question.id, "provider_cancelled");
          };
          const item = { question, settled: false, validate, resolve: resolve2, reject, dispose: () => signal?.removeEventListener("abort", cancelled) };
          this.pending.set(question.id, item);
          signal?.addEventListener("abort", cancelled, { once: true });
          this.publish(question);
        });
      }
      answer(id, response) {
        const item = this.pending.get(id);
        if (!item || item.settled) return { accepted: false, detail: "Question is stale or already has a responder" };
        if (response === null || typeof response !== "object" || Array.isArray(response)) return { accepted: false, detail: "Answer must be a typed object" };
        const q = item.question;
        let valid = false;
        if (response.cancelled === true) valid = true;
        else if (q.kind === "confirm") valid = typeof response.confirmed === "boolean";
        else if (q.kind === "input" || q.kind === "editor") valid = typeof response.text === "string";
        else if (q.kind === "select" || q.kind === "permission") valid = typeof response.option_id === "string" && q.options.some((option) => option.id === response.option_id);
        else if (q.kind === "form") valid = response.values !== null && typeof response.values === "object" && !Array.isArray(response.values) && (!item.validate || item.validate(response.values));
        else if (q.kind === "ask" && Array.isArray(response.answers)) {
          const seen = /* @__PURE__ */ new Set();
          valid = response.answers.length === q.questions.length && response.answers.every((answer) => {
            if (answer === null || typeof answer !== "object" || Array.isArray(answer) || typeof answer.id !== "string" || seen.has(answer.id)) return false;
            seen.add(answer.id);
            const question = q.questions.find((question2) => question2.id === answer.id);
            if (!question || !Array.isArray(answer.selected_options) || !question.multi && answer.selected_options.length > 1) return false;
            const selected = /* @__PURE__ */ new Set();
            return answer.selected_options.every((option) => {
              if (typeof option !== "string" || selected.has(option) || !question.options.some((candidate) => candidate.id === option)) return false;
              selected.add(option);
              return true;
            }) && (answer.custom_input === void 0 || typeof answer.custom_input === "string");
          });
        }
        if (!valid) return { accepted: false, detail: "Answer does not match the current provider question" };
        item.settled = true;
        item.resolve(response);
        return { accepted: true, detail: "Answer queued to the waiting SDK callback; provider consumption is not yet confirmed" };
      }
      closed(id) {
        const item = this.pending.get(id);
        item?.dispose();
        if (item && !item.settled) item.reject(new Error("Provider closed question"));
        this.pending.delete(id);
      }
      closeAll(reason) {
        for (const [id, item] of this.pending) {
          item.dispose();
          if (!item.settled) item.reject(new Error("Native controller closed"));
          this.close(id, reason);
        }
        this.pending.clear();
      }
    };
  }
});

// src/pi.ts
var pi_exports = {};
__export(pi_exports, {
  attachPiSession: () => attachPiSession,
  createAdapter: () => createAdapter,
  createPiUI: () => createPiUI
});
import { randomUUID } from "node:crypto";
async function createAdapter(context) {
  if (context.command.length !== 1 || context.command[0] !== "node") throw new Error('Pi in-process SDK requires command ["node"]; additional CLI arguments are unsupported and were not applied');
  if (Object.keys(process.env).some((key) => process.env[key] !== context.env[key]) || Object.keys(context.env).some((key) => context.env[key] !== process.env[key])) {
    throw new Error("Pi requires the dedicated worker environment to match AdapterContext.env");
  }
  const {
    createAgentSession,
    DefaultResourceLoader,
    getAgentDir,
    hasTrustRequiringProjectResources,
    ProjectTrustStore,
    SessionManager,
    SettingsManager
  } = await import("@earendil-works/pi-coding-agent");
  const agentDir = getAgentDir();
  const ui = createPiUI(context);
  const settingsManager = SettingsManager.create(context.cwd, agentDir, { projectTrusted: false });
  const resourceLoader = new DefaultResourceLoader({ cwd: context.cwd, agentDir, settingsManager });
  const trustStore = new ProjectTrustStore(agentDir);
  await resourceLoader.reload({ resolveProjectTrust: async () => {
    const saved = trustStore.get(context.cwd);
    if (saved !== null) return saved;
    if (!hasTrustRequiringProjectResources(context.cwd)) return false;
    return ui.confirm(
      "Trust this Pi project for this session?",
      `Allow project-local Pi settings, extensions and skills in ${context.cwd}?`
    );
  } });
  const sessionDir = settingsManager.getSessionDir();
  let sessionManager;
  if (context.resumeSessionId !== void 0) {
    const path = SessionManager.findById(context.cwd, context.resumeSessionId, sessionDir);
    if (!path) throw new Error("The requested Pi session ID was not found for this working directory");
    sessionManager = SessionManager.open(path, sessionDir, context.cwd);
    if (sessionManager.getSessionId() !== context.resumeSessionId) {
      throw new Error("Pi resumed session identity does not match the requested session ID");
    }
  } else {
    sessionManager = SessionManager.create(context.cwd, sessionDir);
  }
  const { session, modelFallbackMessage } = await createAgentSession({
    cwd: context.cwd,
    agentDir,
    sessionManager,
    settingsManager,
    resourceLoader
  });
  const adapter = attachPiSession(context, session);
  try {
    await session.bindExtensions({
      uiContext: ui,
      mode: "rpc",
      abortHandler: () => {
        void session.abort().catch(() => {
          context.emit({ type: "notice", level: "error", text: "Pi extension abort failed." });
        });
      },
      onError: () => context.emit({ type: "notice", level: "error", text: "Pi extension failed." })
    });
  } catch (error) {
    await adapter.close();
    throw error;
  }
  if (modelFallbackMessage) context.emit({
    type: "notice",
    level: "warning",
    text: "Pi reports a model selection fallback; check the configured model and host authentication."
  });
  return adapter;
}
function object(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
function createPiUI(context) {
  const unsupported = (method) => {
    throw new Error(`Pi native UI does not support terminal-only ${method}`);
  };
  const notice = (text, level = "info") => {
    context.emit({ type: "notice", level, text });
  };
  async function ask(kind, title, message, options, fields, decode, opts) {
    if (opts?.signal?.aborted) return void 0;
    if (opts?.timeout !== void 0 && (!Number.isFinite(opts.timeout) || opts.timeout < 0)) {
      throw new Error("Pi dialog timeout must be finite and nonnegative");
    }
    const controller = new AbortController();
    const cancel = () => controller.abort();
    opts?.signal?.addEventListener("abort", cancel, { once: true });
    const timer = opts?.timeout === void 0 ? void 0 : setTimeout(cancel, opts.timeout);
    const id = randomUUID();
    let reason = "Pi UI dialog failed";
    try {
      const response = await context.question({
        id,
        kind,
        title,
        message,
        options,
        questions: [],
        fields
      }, controller.signal);
      if (controller.signal.aborted || response === null || object(response) && response.cancelled === true) {
        reason = "Pi UI dialog cancelled";
        return void 0;
      }
      const decoded = decode(response);
      reason = "Pi UI dialog resolved";
      return decoded;
    } catch (error) {
      if (controller.signal.aborted) {
        reason = "Pi UI dialog cancelled";
        return void 0;
      }
      throw error;
    } finally {
      clearTimeout(timer);
      opts?.signal?.removeEventListener("abort", cancel);
      context.emit({ type: "question_closed", id, reason });
    }
  }
  const ui = {
    async confirm(title, message, opts) {
      return await ask("confirm", title, message, [], {}, (answer) => {
        if (!object(answer) || typeof answer.confirmed !== "boolean") throw new Error("Invalid Pi confirm answer");
        return answer.confirmed;
      }, opts) ?? false;
    },
    async select(title, options, opts) {
      return ask("select", title, "", options.map((value) => ({ id: value, label: value })), {}, (answer) => {
        if (!object(answer) || typeof answer.option_id !== "string" || !options.includes(answer.option_id)) {
          throw new Error("Invalid Pi selection answer");
        }
        return answer.option_id;
      }, opts);
    },
    async input(title, placeholder, opts) {
      return ask("input", title, "", [], { initial_value: "", placeholder: placeholder ?? "" }, (answer) => {
        if (!object(answer) || typeof answer.text !== "string") throw new Error("Invalid Pi input answer");
        return answer.text;
      }, opts);
    },
    async editor(title, prefill) {
      return ask("editor", title, "", [], { initial_value: prefill ?? "" }, (answer) => {
        if (!object(answer) || typeof answer.text !== "string") throw new Error("Invalid Pi editor answer");
        return answer.text;
      });
    },
    notify: notice,
    setStatus: (key, text) => notice(`Pi status ${key}: ${text ?? "(cleared)"}`),
    setWorkingMessage: (message) => notice(message ?? "Pi working message cleared."),
    setWorkingVisible: () => unsupported("setWorkingVisible"),
    setWorkingIndicator: () => unsupported("setWorkingIndicator"),
    setHiddenThinkingLabel: () => unsupported("setHiddenThinkingLabel"),
    setWidget: (key, content) => {
      if (typeof content === "function") return unsupported("component widget");
      notice(`Pi widget ${key}: ${content?.join("\n") ?? "(cleared)"}`);
    },
    onTerminalInput: () => unsupported("onTerminalInput"),
    setFooter: () => unsupported("setFooter"),
    setHeader: () => unsupported("setHeader"),
    setTitle: (title) => notice(`Pi title: ${title}`),
    custom: async () => unsupported("custom component"),
    pasteToEditor: () => unsupported("pasteToEditor"),
    setEditorText: () => unsupported("setEditorText"),
    getEditorText: () => unsupported("getEditorText"),
    addAutocompleteProvider: () => unsupported("addAutocompleteProvider"),
    setEditorComponent: () => unsupported("setEditorComponent"),
    getEditorComponent: () => unsupported("getEditorComponent"),
    get theme() {
      return unsupported("theme");
    },
    getAllThemes: () => unsupported("getAllThemes"),
    getTheme: () => unsupported("getTheme"),
    setTheme: () => ({ success: false, error: "Pi native UI does not render terminal themes" }),
    getToolsExpanded: () => unsupported("getToolsExpanded"),
    setToolsExpanded: () => unsupported("setToolsExpanded")
  };
  return ui;
}
function attachPiSession(context, session) {
  const capabilities2 = { prompt: true, interrupt: true, questions: true, follow_up: true };
  let closed = false;
  let turnId;
  const tools = /* @__PURE__ */ new Map();
  const pending = /* @__PURE__ */ new Set();
  const mode = transcriptMode(context.env.PHUX_AGENT_TRANSCRIPT);
  let providerSessionId = session.sessionManager.getSessionId();
  context.identity(providerSessionId);
  const identity = () => {
    const actual = session.sessionManager.getSessionId();
    if (actual !== providerSessionId) {
      providerSessionId = actual;
      context.identity(actual);
      turnId = void 0;
      context.turn(void 0);
      context.closeQuestions("Pi session changed");
    }
  };
  const state = (value) => {
    context.emit({
      type: "state",
      state: value === "running" ? "working" : "done",
      ...turnId === void 0 ? {} : { turn_id: turnId }
    });
  };
  const transcript = (entry) => {
    if (mode !== "off") context.emit({ type: "transcript", entry: transcriptData(context.provider, entry).entry });
  };
  function message(event, final) {
    if (closed) return;
    identity();
    const msg = event.message;
    const leaf = final ? session.sessionManager.getLeafEntry() : void 0;
    const persistedId = leaf?.type === "message" && leaf.message === msg ? leaf.id : void 0;
    const id = msg.role === "assistant" ? msg.responseId ?? persistedId : persistedId;
    if (msg.role === "user" && persistedId !== void 0) {
      turnId = persistedId;
      context.turn(turnId);
      state("running");
    }
    if (!id) return;
    if (msg.role === "assistant") {
      const text = msg.content.filter((block) => block.type === "text").map((block) => block.text).join("\n");
      const thinking = msg.content.filter((block) => block.type === "thinking").map((block) => block.thinking).join("\n");
      if (text || final) transcript({ id, role: "assistant", text, final, truncated: false });
      if (thinking) transcript({ id: `${id}:thinking`, role: "thinking", text: thinking, final, truncated: false });
      if (final && msg.stopReason === "error") context.emit({
        type: "notice",
        level: "error",
        text: "Pi assistant response failed."
      });
    } else if (msg.role === "user") {
      const text = typeof msg.content === "string" ? msg.content : msg.content.filter((block) => block.type === "text").map((block) => block.text).join("\n");
      transcript({ id, role: "user", text, final, truncated: false });
    }
  }
  const unsubscribe = session.subscribe((event) => {
    if (closed) return;
    identity();
    switch (event.type) {
      case "agent_start":
        state("running");
        break;
      case "agent_settled":
        context.closeQuestions(event.aborted ? "Pi run aborted" : "Pi run settled");
        turnId = void 0;
        context.turn(void 0);
        state(event.aborted ? "stopped" : "idle");
        tools.clear();
        break;
      case "message_start":
      case "message_update":
        message(event, false);
        break;
      case "message_end":
        queueMicrotask(() => message(event, true));
        break;
      case "tool_execution_start": {
        const tool = { name: event.toolName, summary: summarizeArgs(event.args) };
        tools.set(event.toolCallId, tool);
        transcript({
          id: event.toolCallId,
          role: "tool",
          text: "",
          final: false,
          truncated: false,
          tool: { ...tool, call_id: event.toolCallId, status: "running", output: "" }
        });
        break;
      }
      case "tool_execution_update":
      case "tool_execution_end": {
        const tool = tools.get(event.toolCallId) ?? { name: event.toolName, summary: "" };
        const result = event.type === "tool_execution_end" ? event.result : event.partialResult;
        const output = mode === "full" && object(result) && Array.isArray(result.content) ? result.content.filter((block) => object(block) && block.type === "text" && typeof block.text === "string").map((block) => block.text).join("\n") : "";
        transcript({
          id: event.toolCallId,
          role: "tool",
          text: "",
          final: event.type === "tool_execution_end",
          truncated: false,
          tool: {
            ...tool,
            call_id: event.toolCallId,
            status: event.type === "tool_execution_end" ? event.isError ? "error" : "ok" : "running",
            output
          }
        });
        if (event.type === "tool_execution_end") tools.delete(event.toolCallId);
        break;
      }
    }
  });
  state("idle");
  return {
    capabilities: capabilities2,
    get providerSessionId() {
      return providerSessionId;
    },
    async prompt(text, operationId) {
      if (closed) return { disposition: "refused", detail: "Pi session is closed." };
      const task = session.prompt(text, {
        streamingBehavior: "followUp",
        source: "interactive",
        preflightResult: (disposition) => context.emit({
          type: "notice",
          level: "info",
          text: `Pi operation ${operationId}: SDK preflight ${disposition}.`
        })
      });
      const observed = task.then(() => {
        identity();
      }, () => {
        if (!closed) {
          context.emit({ type: "notice", level: "error", text: `Pi operation ${operationId} failed in the SDK.` });
        }
      });
      pending.add(observed);
      void observed.finally(() => pending.delete(observed));
      return { disposition: "queued", detail: "Pi SDK prompt invoked; preflight pending, not provider admission." };
    },
    async interrupt(requestedTurnId) {
      if (closed || turnId === void 0 || requestedTurnId !== turnId) {
        return { disposition: "refused", detail: "Pi interrupt does not match the active input entry." };
      }
      context.closeQuestions("Pi interruption requested");
      await session.abort();
      return { disposition: "completed", detail: "Pi SDK abort completed." };
    },
    async close() {
      if (closed) return;
      closed = true;
      context.closeQuestions("Pi session closed");
      context.connection(false);
      try {
        await session.abort();
        await Promise.all(pending);
      } finally {
        unsubscribe();
        session.dispose();
        context.turn(void 0);
      }
    }
  };
}
var init_pi = __esm({
  "src/pi.ts"() {
    "use strict";
    init_transcript();
  }
});

// src/claude.ts
var claude_exports = {};
__export(claude_exports, {
  claudeEnvironment: () => claudeEnvironment,
  createAdapter: () => createAdapter2
});
import { spawn } from "node:child_process";
import { randomUUID as randomUUID2 } from "node:crypto";
import { constants } from "node:fs";
import { access } from "node:fs/promises";
import { isAbsolute } from "node:path";
function claudeEnvironment(source) {
  const env = {};
  for (const [key, value] of Object.entries(source)) {
    if (key.startsWith("PHUX_NATIVE_") || key.startsWith("PHUX_AGENT_HOST_") || /(?:BINDING|DELEGAT(?:ED|ION)).*(?:PROOF|TOKEN|SECRET|CREDENTIAL)/i.test(key) || key === "NODE_OPTIONS" || key === "NODE_PATH" || key === "DEBUG" || key === "DEBUG_CLAUDE_AGENT_SDK" || key === "CLAUDECODE") continue;
    env[key] = value;
  }
  env.PHUX_AGENT_REPORTING = "0";
  return env;
}
function record(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
function boundedJson(value) {
  const encoded = JSON.stringify(value);
  if (encoded === void 0 || Buffer.byteLength(encoded) > 14 * 1024) {
    throw new Error("Claude UI payload exceeds native question bounds");
  }
  return JSON.parse(encoded);
}
function textContent(content) {
  if (typeof content === "string") return keepTail(content, MAX_TOOL_OUTPUT_BYTES);
  if (!Array.isArray(content)) return "";
  let text = "";
  for (const block of content) {
    if (record(block) && block.type === "text" && typeof block.text === "string") {
      text = keepTail(text + (text ? "\n" : "") + block.text, MAX_TOOL_OUTPUT_BYTES);
    }
  }
  return text;
}
function askItems(input) {
  if (!Array.isArray(input.questions) || input.questions.length < 1 || input.questions.length > 4) {
    throw new Error("Unsupported AskUserQuestion questions");
  }
  const items = input.questions.map((item) => {
    if (!record(item) || typeof item.question !== "string" || typeof item.header !== "string" || typeof item.multiSelect !== "boolean" || !Array.isArray(item.options) || item.options.length < 2 || item.options.length > 4) {
      throw new Error("Unsupported AskUserQuestion item");
    }
    const options = item.options.map((option) => {
      if (!record(option) || typeof option.label !== "string" || typeof option.description !== "string" || option.preview !== void 0) {
        throw new Error("AskUserQuestion previews require unsupported provider UI");
      }
      return { id: option.label, label: option.label, description: option.description };
    });
    if (new Set(options.map((option) => option.id)).size !== options.length) {
      throw new Error("Ambiguous AskUserQuestion labels");
    }
    return { id: item.question, question: item.question, header: item.header, options, multi: item.multiSelect };
  });
  if (new Set(items.map((item) => item.id)).size !== items.length) throw new Error("Ambiguous AskUserQuestion keys");
  return items;
}
function askAnswers(items, response) {
  if (!record(response) || !Array.isArray(response.answers) || response.answers.length !== items.length) {
    throw new Error("AskUserQuestion requires every answer");
  }
  const answers = /* @__PURE__ */ Object.create(null);
  for (const answer of response.answers) {
    if (!record(answer) || typeof answer.id !== "string" || Object.hasOwn(answers, answer.id)) {
      throw new Error("Invalid AskUserQuestion answer identity");
    }
    const item = items.find((candidate) => candidate.id === answer.id);
    if (!item || !Array.isArray(answer.selected_options) || !answer.selected_options.every((id) => typeof id === "string" && item.options.some((option) => option.id === id)) || new Set(answer.selected_options).size !== answer.selected_options.length || !item.multi && answer.selected_options.length > 1 || answer.custom_input !== void 0 && typeof answer.custom_input !== "string") {
      throw new Error("Invalid AskUserQuestion option selection");
    }
    const selected = answer.selected_options;
    const custom = typeof answer.custom_input === "string" ? answer.custom_input : "";
    if (selected.length === 0 && custom.trim() === "") throw new Error("Empty AskUserQuestion answer");
    if (!item.multi && selected.length > 0 && custom !== "") throw new Error("Ambiguous single-choice answer");
    answers[answer.id] = [...selected, ...custom ? [custom] : []].join(", ");
  }
  return answers;
}
async function createAdapter2(context, startQuery) {
  if (context.command.length !== 1 || !isAbsolute(context.command[0])) {
    throw new Error("Claude native mode requires one absolute existing native executable path, without CLI flags");
  }
  const executable = context.command[0];
  if (/\.(?:[cm]?js|tsx?|jsx)$/i.test(executable)) throw new Error("Claude native mode requires the compatible native executable, not a JS CLI entry");
  await access(executable, constants.X_OK);
  const [{ query }, { AjvJsonSchemaValidator }, protocol, { default: Ajv }, { default: addFormats }] = await Promise.all([
    import("@anthropic-ai/claude-agent-sdk/core"),
    import("@modelcontextprotocol/sdk/validation/ajv"),
    import("@modelcontextprotocol/sdk/types.js"),
    import("ajv"),
    import("ajv-formats")
  ]);
  const ajv = new Ajv({ strict: true, allErrors: true, validateFormats: true });
  addFormats(ajv);
  ajv.addKeyword({ keyword: "enumNames", schemaType: "array", valid: true });
  const validator = new AjvJsonSchemaValidator(ajv);
  const shutdown = new AbortController();
  const input = new PromptStream();
  const transcript = new Transcript(context);
  const pending = /* @__PURE__ */ new Map();
  const questions = /* @__PURE__ */ new Map();
  let currentTurn;
  let closed = false;
  let exited;
  const notice = (level, text) => context.emit({ type: "notice", level, text });
  const closeQuestion = (id, reason) => {
    const question = questions.get(id);
    if (!question) return;
    questions.delete(id);
    question.signal.removeEventListener("abort", question.abort);
    context.emit({ type: "question_closed", id, reason });
  };
  const openQuestion = (id, signal, toolId) => {
    if (questions.size >= MAX_PENDING || questions.has(id)) throw new Error("Claude question already pending or question limit exceeded");
    const abort = () => closeQuestion(id, "Claude SDK cancelled request");
    questions.set(id, { toolId, submitted: false, signal, abort });
    signal.addEventListener("abort", abort, { once: true });
  };
  const submitted = (id) => {
    const question = questions.get(id);
    if (question) question.submitted = true;
  };
  const canUseTool = async (toolName, toolInput, options2) => {
    const deny = (message) => ({ behavior: "deny", message, toolUseID: options2.toolUseID });
    const signal = AbortSignal.any([options2.signal, shutdown.signal]);
    if (signal.aborted) return deny("Claude permission request cancelled");
    const providerOptions = options2;
    if (record(providerOptions) && providerOptions.requiresUserInteraction === true && toolName !== "AskUserQuestion") {
      notice("warning", "Claude tool requires unsupported native provider UI; denied");
      return deny("Unsupported provider UI");
    }
    try {
      context.emit({ type: "state", state: "blocked", turn_id: currentTurn });
      if (toolName === "AskUserQuestion") {
        const items = askItems(toolInput);
        openQuestion(options2.requestId, signal, options2.toolUseID);
        const response2 = await context.question({
          id: options2.requestId,
          kind: "ask",
          title: options2.title ?? "Claude question",
          message: options2.description ?? "",
          options: [],
          questions: items,
          fields: { tool_use_id: options2.toolUseID, request_id: options2.requestId }
        }, signal);
        submitted(options2.requestId);
        if (signal.aborted || record(response2) && response2.cancelled === true) return deny("Claude question cancelled");
        return { behavior: "allow", toolUseID: options2.toolUseID, updatedInput: { ...toolInput, answers: askAnswers(items, response2) } };
      }
      openQuestion(options2.requestId, signal, options2.toolUseID);
      const response = await context.question({
        id: options2.requestId,
        kind: "permission",
        title: options2.title ?? options2.displayName ?? toolName,
        message: options2.description ?? options2.decisionReason ?? summarizeArgs(toolInput),
        // These are the official PermissionResult.behavior values. No inferred
        // default, persistent permission rule, or input rewrite is offered.
        options: [{ id: "deny", label: "Deny" }, { id: "allow", label: "Allow this tool call" }],
        questions: [],
        fields: boundedJson({
          request_id: options2.requestId,
          tool_use_id: options2.toolUseID,
          tool_name: toolName,
          blocked_path: options2.blockedPath,
          decision_reason: options2.decisionReason,
          default_to_no: options2.defaultToNo,
          suppress_always_allow_rule: options2.suppressAlwaysAllowRule
        })
      }, signal);
      submitted(options2.requestId);
      if (signal.aborted || record(response) && response.cancelled === true) return deny("Claude permission request cancelled");
      if (record(response) && response.option_id === "allow") return { behavior: "allow", toolUseID: options2.toolUseID };
      if (record(response) && response.option_id === "deny") return deny("User denied this Claude tool call");
      return deny("Invalid native Claude permission answer");
    } catch {
      submitted(options2.requestId);
      notice("warning", "Claude tool request denied: cancelled or unsupported native UI/answer");
      return deny("Native UI could not authorize this tool call");
    }
  };
  const onElicitation = async (request, options2) => {
    const signal = AbortSignal.any([options2.signal, shutdown.signal]);
    if (signal.aborted) return { action: "cancel" };
    if (request.mode === "url") {
      notice("warning", "Claude MCP URL elicitation is unsupported; request cancelled");
      return { action: "cancel" };
    }
    try {
      const parsed = protocol.ElicitRequestFormParamsSchema.safeParse({ mode: "form", message: request.message, requestedSchema: request.requestedSchema });
      if (!parsed.success || !request.requestedSchema) throw new Error("Unsupported MCP form");
      const validate = validator.getValidator(request.requestedSchema);
      context.emit({ type: "state", state: "blocked", turn_id: currentTurn });
      openQuestion(options2.requestId, signal);
      const response = await context.question({
        id: options2.requestId,
        kind: "form",
        title: request.title ?? request.serverName,
        message: request.message,
        options: [],
        questions: [],
        fields: boundedJson(request.requestedSchema)
      }, signal);
      submitted(options2.requestId);
      if (signal.aborted || record(response) && response.cancelled === true) return { action: "cancel" };
      if (!record(response) || !record(response.values) || !validate(response.values).valid) {
        notice("warning", "Claude MCP form answer failed provider schema validation");
        return { action: "decline" };
      }
      return protocol.ElicitResultSchema.parse({ action: "accept", content: response.values });
    } catch {
      submitted(options2.requestId);
      notice("warning", "Claude MCP elicitation cancelled: unsupported schema, UI, or answer");
      return { action: "cancel" };
    }
  };
  const options = {
    pathToClaudeCodeExecutable: executable,
    cwd: context.cwd,
    env: claudeEnvironment(context.env),
    ...context.resumeSessionId ? { resume: context.resumeSessionId } : {},
    abortController: shutdown,
    permissionMode: "default",
    includePartialMessages: true,
    canUseTool,
    onElicitation,
    // No terminal-only dialog kinds are advertised. The SDK fails closed on
    // absence; an unexpected delivery is explicitly cancelled, never approved.
    supportedDialogKinds: [],
    onUserDialog: async () => ({ behavior: "cancelled" }),
    // This release's sdk.d.ts references but does not declare SpawnOptions;
    // keep the concrete published spawn seam typed instead of inheriting it.
    spawnClaudeCodeProcess: (spawnOptions) => {
      if (spawnOptions.command !== executable) throw new Error("Claude SDK attempted a different executable");
      const child = spawn(executable, spawnOptions.args, {
        cwd: spawnOptions.cwd,
        env: claudeEnvironment(spawnOptions.env),
        signal: spawnOptions.signal,
        detached: false,
        windowsHide: true,
        stdio: ["pipe", "pipe", "pipe"]
      });
      child.stderr.resume();
      const { promise, resolve: resolve2 } = Promise.withResolvers();
      child.once("close", () => resolve2());
      child.once("error", () => resolve2());
      exited = promise;
      return child;
    }
  };
  const conversation = (startQuery ?? query)({ prompt: input, options });
  const adapter = {
    capabilities: { prompt: true, interrupt: true, questions: true, follow_up: true },
    async prompt(text, operationId) {
      if (closed) return { disposition: "refused", detail: "Claude query is closed" };
      if (pending.size >= MAX_PENDING) return { disposition: "refused", detail: "Claude has 64 unconsumed prompts" };
      const uuid = randomUUID2();
      pending.set(uuid, operationId);
      input.push({
        type: "user",
        uuid,
        message: { role: "user", content: text },
        parent_tool_use_id: null,
        origin: { kind: "human" },
        ...adapter.providerSessionId ? { session_id: adapter.providerSessionId } : {}
      });
      transcript.emit({ id: uuid, role: "user", text, truncated: false, final: true });
      return { disposition: "queued", detail: `Queued in Claude SDK streaming input (${uuid}); provider consumption not yet confirmed` };
    },
    async interrupt(turnId) {
      if (closed || !currentTurn || currentTurn !== turnId) return { disposition: "refused", detail: "Claude turn is no longer current" };
      try {
        const receipt = await conversation.interrupt();
        context.closeQuestions("Claude acknowledged interrupt");
        return { disposition: "completed", detail: receipt ? `Claude acknowledged interrupt; ${receipt.still_queued.length} queued prompts remain. Turn termination is reported separately.` : "Claude acknowledged interrupt; turn termination is reported separately" };
      } catch {
        return { disposition: "unknown", detail: "Claude interrupt outcome is unknown; it was not resent" };
      }
    },
    async close() {
      if (!closed) {
        closed = true;
        input.finish();
        shutdown.abort();
        context.closeQuestions("Claude query closed");
        conversation.close();
      }
      await draining;
      await exited;
    }
  };
  const draining = (async () => {
    try {
      for await (const message of conversation) {
        if (message.session_id && message.session_id !== adapter.providerSessionId) {
          adapter.providerSessionId = message.session_id;
          context.identity(message.session_id);
        }
        const outcomeToolIds = [];
        if (message.type === "tool_progress") outcomeToolIds.push(message.tool_use_id);
        else if (message.type === "system" && message.subtype === "permission_denied") outcomeToolIds.push(message.tool_use_id);
        else if (message.type === "user" && typeof message.message.content !== "string") {
          for (const block of message.message.content) if (block.type === "tool_result") outcomeToolIds.push(block.tool_use_id);
        }
        for (const [id, question] of questions) {
          if (!question.submitted) continue;
          if (question.toolId && outcomeToolIds.includes(question.toolId)) {
            closeQuestion(id, "Claude SDK reported the requested tool outcome");
          } else if (!question.toolId && (message.type === "assistant" || message.type === "stream_event" || message.type === "tool_progress")) {
            closeQuestion(id, "Claude SDK continued after submitted elicitation");
          } else if (message.type === "result") {
            closeQuestion(id, "Claude SDK turn ended after submitted response");
          }
        }
        if ((message.type === "assistant" || message.type === "stream_event") && message.parent_tool_use_id === null) {
          if (message.user_message_uuid) {
            currentTurn = message.user_message_uuid;
            context.turn(currentTurn);
          }
          for (const id of message.user_message_uuids ?? (message.user_message_uuid ? [message.user_message_uuid] : [])) {
            const operation = pending.get(id);
            if (operation !== void 0) {
              pending.delete(id);
              notice("info", `Claude consumed prompt operation ${operation} (${id})`);
            }
          }
          let waitingForAnswer = false;
          for (const question of questions.values()) {
            if (!question.submitted) {
              waitingForAnswer = true;
              break;
            }
          }
          context.emit({ type: "state", state: waitingForAnswer ? "blocked" : "working", turn_id: currentTurn });
        }
        transcript.message(message);
        if (message.type === "result") {
          for (const id of message.user_message_uuids ?? (message.user_message_uuid ? [message.user_message_uuid] : [])) pending.delete(id);
          if (message.is_error) notice("error", `Claude turn ended with ${message.subtype}`);
          currentTurn = void 0;
          context.turn(void 0);
          context.emit({ type: "state", state: "done" });
        } else if (message.type === "system" && message.subtype === "session_state_changed") {
          if (message.state === "idle") {
            currentTurn = void 0;
            context.turn(void 0);
          }
          context.emit({ type: "state", state: message.state === "idle" ? "done" : message.state === "requires_action" ? "blocked" : "working", turn_id: currentTurn });
        }
      }
      if (!closed) notice("warning", "Claude SDK stream ended; native control is closed");
    } catch {
      if (!closed) notice("error", "Claude SDK stream failed; native control is closed");
    } finally {
      closed = true;
      context.connection(false);
      input.finish();
      pending.clear();
      shutdown.abort();
      context.closeQuestions("Claude SDK stream closed");
      context.turn(void 0);
      currentTurn = void 0;
      context.emit({ type: "state", state: "done" });
    }
  })();
  try {
    const initialized = await conversation.initializationResult();
    if (initialized.claude_code_version !== CLAUDE_VERSION) {
      throw new Error(`Claude SDK 0.3.300 requires a verified Claude Code ${CLAUDE_VERSION} executable`);
    }
    if (closed) throw new Error("Claude query ended during initialization");
    context.connection(true);
    return adapter;
  } catch (error) {
    await adapter.close();
    throw error;
  }
}
var CLAUDE_VERSION, MAX_PENDING, MAX_TEXT_BYTES, PromptStream, Transcript;
var init_claude = __esm({
  "src/claude.ts"() {
    "use strict";
    init_transcript();
    CLAUDE_VERSION = "2.1.300";
    MAX_PENDING = 64;
    MAX_TEXT_BYTES = 16 * 1024;
    PromptStream = class {
      queued = [];
      waiting;
      ended = false;
      push(message) {
        if (this.ended) throw new Error("Claude input is closed");
        if (this.waiting) {
          const resolve2 = this.waiting;
          this.waiting = void 0;
          resolve2({ done: false, value: message });
        } else this.queued.push(message);
      }
      finish() {
        this.ended = true;
        this.queued.length = 0;
        this.waiting?.({ done: true, value: void 0 });
        this.waiting = void 0;
      }
      [Symbol.asyncIterator]() {
        return {
          next: () => {
            const message = this.queued.shift();
            if (message) return Promise.resolve({ done: false, value: message });
            if (this.ended) return Promise.resolve({ done: true, value: void 0 });
            const { promise, resolve: resolve2 } = Promise.withResolvers();
            this.waiting = resolve2;
            return promise;
          },
          return: async () => {
            this.finish();
            return { done: true, value: void 0 };
          }
        };
      }
    };
    Transcript = class {
      constructor(context) {
        this.context = context;
        this.mode = transcriptMode(context.env.PHUX_AGENT_TRANSCRIPT);
      }
      context;
      gate = new TranscriptGate();
      streams = /* @__PURE__ */ new Map();
      streamed = /* @__PURE__ */ new Set();
      tools = /* @__PURE__ */ new Map();
      mode;
      emit(entry) {
        if (this.mode === "off") return;
        const normalized = transcriptData("claude", entry).entry;
        if (entry.role === "tool" || this.gate.admit(normalized)) this.context.emit({ type: "transcript", entry: normalized });
      }
      tool(id, name, summary, status, output = "") {
        const row = { name, call_id: id, summary, status, output: this.mode === "full" ? output : "" };
        const previous = this.tools.get(id);
        if (previous?.name === name && previous.summary === row.summary && previous.status === status && previous.output === row.output) return;
        if (this.tools.size >= 512 && !this.tools.has(id)) this.tools.delete(this.tools.keys().next().value);
        this.tools.set(id, row);
        this.emit({ id, role: "tool", text: "", truncated: false, final: status !== "running", tool: row });
      }
      stream(message) {
        const key = message.parent_tool_use_id ?? "";
        const event = message.event;
        if (event.type === "message_start") {
          if (this.streams.size >= 64 && !this.streams.has(key)) throw new Error("Too many Claude content streams");
          this.streams.set(key, { id: event.message.id, blocks: /* @__PURE__ */ new Map() });
          this.streamed.add(event.message.id);
          if (this.streamed.size > 512) this.streamed.delete(this.streamed.values().next().value);
          return;
        }
        const stream = this.streams.get(key);
        if (!stream) return;
        if (event.type === "content_block_start") {
          const block = event.content_block;
          if (block.type === "text" || block.type === "thinking") {
            if (stream.blocks.size >= 256 && !stream.blocks.has(event.index)) throw new Error("Too many Claude content blocks");
            const original = block.type === "text" ? block.text : block.thinking;
            const text = keepHead(original, MAX_TEXT_BYTES);
            stream.blocks.set(event.index, {
              role: block.type === "text" ? "assistant" : "thinking",
              text,
              truncated: original.length > text.length
            });
          } else if (block.type === "tool_use" || block.type === "server_tool_use") {
            this.tool(block.id, block.name, summarizeArgs(block.input), "running");
          }
        } else if (event.type === "content_block_delta") {
          const block = stream.blocks.get(event.index);
          const delta = event.delta;
          if (block && (delta.type === "text_delta" || delta.type === "thinking_delta")) {
            const combined = block.text + (delta.type === "text_delta" ? delta.text : delta.thinking);
            block.text = keepHead(combined, MAX_TEXT_BYTES);
            block.truncated ||= combined.length > block.text.length;
            this.emit({ id: `${stream.id}:${event.index}`, role: block.role, text: block.text, truncated: block.truncated, final: false });
          }
        } else if (event.type === "content_block_stop") {
          const block = stream.blocks.get(event.index);
          if (block) this.emit({ id: `${stream.id}:${event.index}`, role: block.role, text: block.text, truncated: block.truncated, final: true });
        } else if (event.type === "message_stop") {
          for (const [index, block] of stream.blocks) {
            this.emit({ id: `${stream.id}:${index}`, role: block.role, text: block.text, truncated: block.truncated, final: true });
          }
          this.streams.delete(key);
        }
      }
      message(message) {
        if (message.type === "stream_event") this.stream(message);
        else if (message.type === "assistant") {
          if (message.aborted) {
            for (const [key, stream] of this.streams) {
              if (stream.id !== message.message.id) continue;
              for (const [index, block] of stream.blocks) {
                this.emit({ id: `${stream.id}:${index}`, role: block.role, text: block.text, truncated: true, final: true });
              }
              this.streams.delete(key);
            }
          }
          for (const [index, block] of message.message.content.entries()) {
            if (block.type === "text" || block.type === "thinking") {
              if (!this.streamed.has(message.message.id)) {
                this.emit({
                  id: `${message.uuid}:${index}`,
                  role: block.type === "text" ? "assistant" : "thinking",
                  text: block.type === "text" ? block.text : block.thinking,
                  truncated: message.aborted === true,
                  final: true
                });
              }
            } else if (block.type === "tool_use" || block.type === "server_tool_use") {
              this.tool(block.id, block.name, summarizeArgs(block.input), "running");
            }
          }
          if (message.error) this.context.emit({ type: "notice", level: "error", text: `Claude assistant error: ${message.error}` });
        } else if (message.type === "user") {
          if (typeof message.message.content !== "string") {
            for (const block of message.message.content) {
              if (block.type === "tool_result") {
                const previous = this.tools.get(block.tool_use_id);
                this.tool(
                  block.tool_use_id,
                  previous?.name ?? "Tool",
                  previous?.summary ?? "",
                  block.is_error ? "error" : "ok",
                  textContent(block.content)
                );
              }
            }
          }
        } else if (message.type === "tool_progress") {
          const previous = this.tools.get(message.tool_use_id);
          this.tool(message.tool_use_id, message.tool_name, previous?.summary ?? "", "running");
        } else if (message.type === "system" && message.subtype === "local_command_output") {
          this.emit({ id: message.uuid, role: "system", text: message.content, truncated: false, final: true });
        }
      }
    };
  }
});

// src/opencode.ts
var opencode_exports = {};
__export(opencode_exports, {
  OpenCodeAdapter: () => OpenCodeAdapter,
  createAdapter: () => createAdapter3,
  formSchema: () => formSchema,
  privateServiceLaunch: () => privateServiceLaunch,
  validateFormAnswer: () => validateFormAnswer
});
import { spawn as spawn2 } from "node:child_process";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
async function createAdapter3(context) {
  if (!context.command[0]) throw new Error("OpenCode native mode requires an explicit executable");
  const [{ OpenCode }, { Service }] = await Promise.all([import("@opencode/client"), import("@opencode/client/service")]);
  const directory = await mkdtemp(join(tmpdir(), "phux-opencode-"));
  const launch = privateServiceLaunch(context, directory);
  const child = spawn2(launch.command, launch.args, {
    cwd: context.cwd,
    env: launch.env,
    detached: false,
    stdio: "ignore",
    windowsHide: true
  });
  let exited = false;
  let failed = false;
  const reaped = new Promise((resolve2) => {
    child.once("error", () => {
      failed = true;
      exited = true;
      resolve2();
    });
    child.once("close", () => {
      exited = true;
      resolve2();
    });
  });
  let stopped = false;
  const stop = async () => {
    if (stopped) return;
    stopped = true;
    if (!exited) {
      child.kill("SIGTERM");
      await Promise.race([reaped, delay(5e3, void 0, { ref: false })]);
      if (!exited) child.kill("SIGKILL");
    }
    await reaped;
    await rm(directory, { recursive: true, force: true });
  };
  try {
    const deadline = Date.now() + 3e4;
    while (Date.now() < deadline) {
      if (exited) throw new Error(failed ? "OpenCode executable could not start" : "OpenCode private service exited before readiness");
      const registration = await readFile(launch.file, "utf8").then((text) => JSON.parse(text)).catch(() => void 0);
      if (record2(registration) && registration.pid === child.pid) {
        const endpoint = await Service.discover({ file: launch.file, version: VERSION });
        if (endpoint) {
          const client = OpenCode.make({ baseUrl: endpoint.url, headers: Service.headers(endpoint) });
          const adapter = new OpenCodeAdapter(context, client, stop);
          await adapter.start();
          void reaped.then(() => adapter.serviceExited());
          return adapter;
        }
      }
      await delay(100);
    }
    throw new Error("OpenCode 2.0.26 private service did not become ready");
  } catch (error) {
    await stop();
    throw error;
  }
}
function privateServiceLaunch(context, directory) {
  const [command2, ...prefix] = context.command;
  if (!command2) throw new Error("OpenCode native mode requires an explicit executable");
  const state = join(directory, "state");
  const env = {
    ...context.env,
    XDG_STATE_HOME: state,
    OPENCODE_CONFIG_DIR: join(directory, "config"),
    PHUX_AGENT_REPORTING: "0"
  };
  delete env.PHUX_NATIVE_BINDING;
  delete env.PHUX_NATIVE_BINDING_PROOF;
  delete env.PHUX_NATIVE_ACTIVATION;
  delete env.PHUX_NATIVE_AGENT_ID;
  delete env.OPENCODE_PTY_HANDOFF;
  return {
    command: command2,
    args: [...prefix, "serve", "--service", "--hostname", "127.0.0.1", "--port", "0"],
    file: join(state, "opencode", "service.json"),
    env
  };
}
function mutationFailure(error, detail) {
  return { disposition: record2(error) && typeof error._tag === "string" ? "refused" : "unknown", detail };
}
function record2(value) {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
function formSchema(fields) {
  const properties = {};
  const required = [];
  for (const field of fields) {
    if (field.type === "external" || field.hidden || field.when?.length || Object.hasOwn(properties, field.key)) throw new Error("Unsupported OpenCode form presentation");
    const schema2 = { type: field.type === "multiselect" ? "array" : field.type, title: field.title ?? field.key };
    if (field.description !== void 0) schema2.description = field.description;
    if (field.default !== void 0) schema2.default = field.default;
    if (field.required) required.push(field.key);
    if (field.type === "string") {
      if (field.minLength !== void 0) schema2.minLength = field.minLength;
      if (field.maxLength !== void 0) schema2.maxLength = field.maxLength;
      if (field.pattern !== void 0) {
        new RegExp(field.pattern);
        schema2.pattern = field.pattern;
      }
      if (field.format !== void 0) schema2.format = field.format;
      if (field.options && !field.custom) schema2.oneOf = field.options.map((option) => ({ const: option.value, title: option.label, ...option.description ? { description: option.description } : {} }));
    } else if (field.type === "number" || field.type === "integer") {
      if (field.default !== void 0 && (typeof field.default !== "number" || !Number.isFinite(field.default))) throw new Error("Nonfinite form default");
      if (field.minimum !== void 0) {
        if (typeof field.minimum !== "number" || !Number.isFinite(field.minimum)) throw new Error("Nonfinite form bound");
        schema2.minimum = field.minimum;
      }
      if (field.maximum !== void 0) {
        if (typeof field.maximum !== "number" || !Number.isFinite(field.maximum)) throw new Error("Nonfinite form bound");
        schema2.maximum = field.maximum;
      }
    } else if (field.type === "multiselect") {
      schema2.items = field.custom ? { type: "string" } : { type: "string", oneOf: field.options.map((option) => ({ const: option.value, title: option.label, ...option.description ? { description: option.description } : {} })) };
      if (field.minItems !== void 0) schema2.minItems = field.minItems;
      if (field.maxItems !== void 0) schema2.maxItems = field.maxItems;
    }
    Object.defineProperty(properties, field.key, { value: schema2, enumerable: true });
  }
  const schema = { type: "object", properties, required, additionalProperties: false };
  if (Buffer.byteLength(JSON.stringify(schema), "utf8") > 10 * 1024) throw new Error("OpenCode form exceeds native payload bound");
  return schema;
}
function validateFormAnswer(fields, response) {
  if (!record2(response) || !record2(response.values)) throw new Error("Expected OpenCode form values");
  const values = response.values;
  const answer = {};
  if (Object.keys(values).some((key) => !fields.some((field) => field.key === key))) throw new Error("Unknown OpenCode form field");
  for (const field of fields) {
    if (field.type === "external") throw new Error("External form fields are unsupported");
    const value = Object.hasOwn(values, field.key) ? values[field.key] : void 0;
    if (value === void 0) {
      if (field.required) throw new Error("Required OpenCode form field missing");
      continue;
    }
    if (field.required && (value === "" || Array.isArray(value) && value.length === 0)) throw new Error("Required OpenCode form field empty");
    if (field.type === "string") {
      if (typeof value !== "string" || field.minLength !== void 0 && value.length < field.minLength || field.maxLength !== void 0 && value.length > field.maxLength || field.pattern !== void 0 && !new RegExp(field.pattern).test(value)) throw new Error("Invalid OpenCode text field");
      if (field.options && !field.custom && !field.options.some((option) => option.value === value)) throw new Error("Unknown OpenCode form option");
      if (field.format === "email" && !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(value)) throw new Error("Invalid email");
      if (field.format === "uri") new URL(value);
      if (field.format === "date" && (!/^\d{4}-\d{2}-\d{2}$/.test(value) || Number.isNaN(Date.parse(`${value}T00:00:00.000Z`)) || (/* @__PURE__ */ new Date(`${value}T00:00:00.000Z`)).toISOString().slice(0, 10) !== value)) throw new Error("Invalid date");
      if (field.format === "date-time" && Number.isNaN(Date.parse(value))) throw new Error("Invalid date-time");
    } else if (field.type === "number" || field.type === "integer") {
      if (typeof value !== "number" || !Number.isFinite(value) || field.type === "integer" && !Number.isInteger(value) || typeof field.minimum === "number" && value < field.minimum || typeof field.maximum === "number" && value > field.maximum) throw new Error("Invalid OpenCode number field");
    } else if (field.type === "boolean") {
      if (typeof value !== "boolean") throw new Error("Invalid OpenCode boolean field");
    } else {
      if (!Array.isArray(value) || !value.every((item) => typeof item === "string") || field.minItems !== void 0 && value.length < field.minItems || field.maxItems !== void 0 && value.length > field.maxItems || !field.custom && value.some((item) => !field.options.some((option) => option.value === item))) throw new Error("Invalid OpenCode multiselect field");
    }
    if (typeof value === "string" || typeof value === "number" || typeof value === "boolean" || Array.isArray(value) && value.every((item) => typeof item === "string")) Object.defineProperty(answer, field.key, { value, enumerable: true });
    else throw new Error("Invalid OpenCode form value");
  }
  return answer;
}
var VERSION, capabilities, OpenCodeAdapter;
var init_opencode = __esm({
  "src/opencode.ts"() {
    "use strict";
    init_transcript();
    VERSION = "2.0.26";
    capabilities = { prompt: true, interrupt: true, questions: true, follow_up: true };
    OpenCodeAdapter = class {
      constructor(context, client, stopService) {
        this.context = context;
        this.client = client;
        this.stopService = stopService;
      }
      context;
      client;
      stopService;
      capabilities = capabilities;
      providerSessionId;
      lifetime = new AbortController();
      pending = /* @__PURE__ */ new Map();
      transcript = /* @__PURE__ */ new Map();
      unsupported = /* @__PURE__ */ new Set();
      turnId;
      sequence;
      running = false;
      connected = false;
      streamReady = false;
      stream;
      dirty = false;
      refreshing;
      events;
      closed = false;
      async start() {
        const session = this.context.resumeSessionId ? await this.client.session.get({ sessionID: this.context.resumeSessionId }, this.request()) : await this.client.session.create({ location: { directory: this.context.cwd } }, this.request());
        if (this.context.resumeSessionId && session.id !== this.context.resumeSessionId) throw new Error("OpenCode returned a different session than the requested resume ID");
        this.providerSessionId = session.id;
        this.context.identity(session.id);
        this.events = this.drain();
        try {
          await this.refresh();
        } catch (error) {
          await this.close();
          throw error;
        }
      }
      request() {
        return { signal: AbortSignal.any([this.lifetime.signal, AbortSignal.timeout(3e4)]) };
      }
      sessionID() {
        if (!this.providerSessionId) throw new Error("OpenCode session is not initialized");
        return this.providerSessionId;
      }
      view() {
        this.context.connection(this.connected && !this.closed);
        this.context.emit({ type: "view", view: {
          activation: this.context.activation,
          backend: "node",
          capabilities,
          connected: this.connected && !this.closed,
          provider_session_id: this.providerSessionId,
          turn_id: this.turnId,
          questions: [...this.pending.values()].map((item) => item.question)
        } });
      }
      state() {
        this.context.turn(this.turnId);
        this.context.emit({ type: "state", state: this.pending.size ? "blocked" : this.running ? "working" : "done", turn_id: this.turnId });
        this.view();
      }
      notice(text) {
        this.context.emit({ type: "notice", level: "error", text });
      }
      async drain() {
        while (!this.closed) {
          this.streamReady = false;
          const stream = new AbortController();
          this.stream = stream;
          try {
            for await (const event of this.client.event.subscribe({ signal: AbortSignal.any([this.lifetime.signal, stream.signal]) })) {
              if (this.closed) break;
              if (event.type === "server.connected") {
                this.streamReady = true;
                this.scheduleRefresh();
                continue;
              }
              if ("data" in event && record2(event.data) && ("sessionID" in event.data && event.data.sessionID === this.providerSessionId || event.type === "form.created" && record2(event.data.form) && event.data.form.sessionID === this.providerSessionId)) this.scheduleRefresh();
            }
          } catch {
            if (this.closed) return;
          }
          if (this.closed) return;
          this.streamReady = false;
          this.connected = false;
          this.view();
          try {
            await delay(250, void 0, { signal: this.lifetime.signal });
          } catch {
            return;
          }
        }
      }
      scheduleRefresh() {
        void this.refresh().catch(() => {
          if (!this.closed) {
            this.connected = false;
            this.view();
            this.notice("OpenCode session snapshot unavailable; no input was replayed");
            this.stream?.abort();
          }
        });
      }
      refresh() {
        this.dirty = true;
        if (this.refreshing) return this.refreshing;
        this.refreshing = (async () => {
          while (this.dirty && !this.closed) {
            this.dirty = false;
            await this.snapshot();
          }
        })().finally(() => {
          this.refreshing = void 0;
        });
        return this.refreshing;
      }
      async snapshot() {
        const sessionID = this.sessionID();
        for await (const event of this.client.session.log({ sessionID, after: this.sequence, follow: false }, this.request())) {
          if (event.type === "log.synced") break;
          this.sequence = Math.max(this.sequence ?? 0, event.durable.seq);
          if (event.type === "session.execution.started") this.turnId = event.id;
          if (event.type === "session.execution.succeeded" || event.type === "session.execution.failed" || event.type === "session.execution.interrupted") {
            this.turnId = void 0;
            if (event.type === "session.execution.failed") this.notice("OpenCode execution failed");
          }
        }
        const [session, active, permissions, forms] = await Promise.all([
          this.client.session.get({ sessionID }, this.request()),
          this.client.session.active(this.request()),
          this.client.permission.list({ sessionID }, this.request()),
          this.client.session.form.list({ sessionID }, this.request())
        ]);
        if (session.id !== sessionID) throw new Error("OpenCode returned a different session");
        const details = await Promise.all(forms.map((form) => this.client.session.form.get({ sessionID, formID: form.id }, this.request())));
        let cursor;
        do {
          const messages = await this.client.message.list({ sessionID, order: "asc", limit: 100, cursor }, this.request());
          if (this.closed) return;
          for (const message of messages.data) this.publishMessage(message);
          const next = messages.cursor.next ?? void 0;
          if (next === cursor && next !== void 0) throw new Error("OpenCode returned a repeated message cursor");
          cursor = next;
        } while (cursor !== void 0);
        if (this.closed) return;
        this.connected = this.streamReady;
        this.running = active[sessionID]?.type === "running";
        if (!this.running) this.turnId = void 0;
        const current = /* @__PURE__ */ new Set([...permissions.map((item) => item.id), ...details.filter((item) => item.state.status === "pending").map((item) => item.id)]);
        for (const id of this.pending.keys()) if (!current.has(id)) this.closeQuestion(id, "Provider no longer has this request pending");
        for (const permission of permissions) this.openPermission(permission);
        for (const form of details) if (form.state.status === "pending") this.openForm(form);
        this.state();
      }
      publish(entry) {
        const bounded = transcriptData(this.context.provider, entry).entry;
        const fingerprint = JSON.stringify(bounded);
        if (this.transcript.get(entry.id) === fingerprint) return;
        this.transcript.set(entry.id, fingerprint);
        this.context.emit({ type: "transcript", entry: bounded });
      }
      publishMessage(message) {
        if (message.type === "assistant") {
          const final = message.time.completed !== void 0;
          const text = message.content.filter((item) => item.type === "text").map((item) => item.text).join("");
          this.publish({ id: message.id, role: "assistant", text, truncated: false, final });
          const thinking = message.content.filter((item) => item.type === "reasoning").map((item) => item.text).join("");
          if (thinking) this.publish({ id: `${message.id}:reasoning`, role: "thinking", text: thinking, truncated: false, final });
          for (const item of message.content) {
            if (item.type !== "tool") continue;
            const state = item.state;
            const finished = state.status === "completed" || state.status === "error";
            const output = finished ? (state.content ?? []).flatMap((part) => part.type === "text" && "text" in part ? [part.text] : []).join("\n") : "";
            this.publish({
              id: item.id,
              role: "tool",
              text: state.status === "error" ? state.error.message : "",
              truncated: false,
              final: finished,
              tool: { name: item.name, call_id: item.id, summary: summarizeArgs(state.input), status: state.status === "error" ? "error" : finished ? "ok" : "running", output }
            });
          }
        } else if (message.type === "user" || message.type === "system" || message.type === "synthetic" || message.type === "skill") {
          this.publish({ id: message.id, role: message.type === "user" ? "user" : "system", text: message.text, truncated: false, final: true });
        } else if (message.type === "shell") {
          this.publish({
            id: message.id,
            role: "tool",
            text: "",
            truncated: message.output?.truncated ?? false,
            final: message.status !== "running",
            tool: { name: "shell", call_id: message.shellID, summary: message.command, status: message.status === "running" ? "running" : message.status === "exited" && message.exit === 0 ? "ok" : "error", output: message.output?.output ?? "" }
          });
        }
      }
      closeQuestion(id, reason) {
        const item = this.pending.get(id);
        if (!item) return;
        this.pending.delete(id);
        item.controller.abort();
        this.context.emit({ type: "question_closed", id, reason });
      }
      open(question, reply) {
        if (this.pending.has(question.id)) return;
        if (Buffer.byteLength(JSON.stringify(question), "utf8") > 12 * 1024) {
          this.notice("OpenCode request exceeds the native question payload bound; it was not approved");
          return;
        }
        const pending = { question, controller: new AbortController() };
        this.pending.set(question.id, pending);
        void this.context.question(question, pending.controller.signal).then(async (answer) => {
          if (this.closed || this.pending.get(question.id) !== pending) return;
          await reply(answer);
          if (this.closed || this.pending.get(question.id) !== pending) return;
          this.closeQuestion(question.id, "OpenCode acknowledged the response");
          this.scheduleRefresh();
        }).catch(() => {
          if (this.closed || pending.controller.signal.aborted) return;
          this.pending.delete(question.id);
          this.notice("OpenCode did not acknowledge the answer; refreshing the actual pending request, without resending the answer");
          this.scheduleRefresh();
        });
      }
      openPermission(permission) {
        if (permission.sessionID !== this.sessionID()) return;
        this.open({
          id: permission.id,
          kind: "permission",
          title: permission.action,
          message: [permission.message, ...permission.resources].filter((item) => item !== void 0).join("\n"),
          // Exact API decision IDs and English labels from v2.0.26
          // packages/tui/src/util/permission.ts:permissionOptionLabel. No default.
          options: [{ id: "once", label: "Allow once" }, { id: "always", label: "Always allow" }, { id: "reject", label: "Reject" }],
          questions: [],
          fields: { action: permission.action, resources: permission.resources, save: permission.save ?? [], source: permission.source ? { type: permission.source.type, messageID: permission.source.messageID, id: permission.source.id } : null }
        }, async (answer) => {
          if (record2(answer) && answer.cancelled === true) {
            await this.client.permission.reply({ sessionID: this.sessionID(), requestID: permission.id, decision: "reject" }, this.request());
            return;
          }
          if (!record2(answer) || answer.option_id !== "once" && answer.option_id !== "always" && answer.option_id !== "reject") throw new Error("Invalid OpenCode permission decision");
          await this.client.permission.reply({ sessionID: this.sessionID(), requestID: permission.id, decision: answer.option_id }, this.request());
        });
      }
      openForm(form) {
        if (form.sessionID !== this.sessionID() || this.pending.has(form.id) || this.unsupported.has(form.id)) return;
        let fields;
        try {
          fields = formSchema(form.fields);
        } catch {
          this.unsupported.add(form.id);
          this.notice("OpenCode requested an unsupported external, conditional, hidden or oversized form; cancelling it without approval");
          void this.client.session.form.cancel({ sessionID: this.sessionID(), formID: form.id, message: "This native client cannot safely display the requested form" }, this.request()).then(() => this.scheduleRefresh()).catch(() => {
            this.unsupported.delete(form.id);
            this.notice("OpenCode did not acknowledge cancellation of the unsupported form");
          });
          return;
        }
        this.open({ id: form.id, kind: "form", title: form.title, message: "", options: [], questions: [], fields }, async (answer) => {
          if (record2(answer) && answer.cancelled === true) {
            await this.client.session.form.cancel({ sessionID: this.sessionID(), formID: form.id, message: "Cancelled by the user" }, this.request());
            return;
          }
          const values = validateFormAnswer(form.fields, answer);
          await this.client.session.form.reply({ sessionID: this.sessionID(), formID: form.id, answer: values }, this.request());
        });
      }
      async prompt(text, operationId) {
        if (this.closed || !this.connected) return { disposition: "refused", detail: "OpenCode event connection is unavailable" };
        try {
          const receipt = await this.client.session.prompt({ sessionID: this.sessionID(), text }, this.request());
          if (receipt.sessionID !== this.sessionID()) return { disposition: "unknown", detail: "OpenCode returned an unexpected session for prompt admission" };
          this.scheduleRefresh();
          return { disposition: "queued", detail: `OpenCode inbox ${receipt.id}; delivery=${receipt.delivery}; host operation=${operationId}. Not a completed model turn.` };
        } catch (error) {
          return mutationFailure(error, "OpenCode prompt admission was not acknowledged; it was not retried");
        }
      }
      async interrupt(turnId) {
        if (this.closed || !this.connected) return { disposition: "refused", detail: "OpenCode event connection is unavailable" };
        try {
          await this.refresh();
        } catch {
          return { disposition: "refused", detail: "Cannot establish the current OpenCode execution" };
        }
        if (!this.running || !this.turnId || turnId !== this.turnId) return { disposition: "refused", detail: "OpenCode execution ID is no longer current" };
        try {
          const result = await this.client.session.interrupt({ sessionID: this.sessionID() }, this.request());
          this.scheduleRefresh();
          return result.interrupted ? { disposition: "completed", detail: "OpenCode acknowledged execution interruption" } : { disposition: "refused", detail: "OpenCode reported no execution was interrupted" };
        } catch (error) {
          return mutationFailure(error, "OpenCode interruption was not acknowledged; it was not retried");
        }
      }
      serviceExited() {
        if (this.closed) return;
        this.closed = true;
        this.connected = false;
        this.lifetime.abort();
        for (const id of this.pending.keys()) this.closeQuestion(id, "OpenCode private service exited");
        this.context.closeQuestions("OpenCode private service exited");
        this.notice("OpenCode private service exited; native control is unavailable");
        this.view();
      }
      async close() {
        this.closed = true;
        this.connected = false;
        this.lifetime.abort();
        for (const id of this.pending.keys()) this.closeQuestion(id, "OpenCode native host closed");
        this.context.closeQuestions("OpenCode native host closed");
        this.view();
        await this.stopService();
        await this.events;
        await this.refreshing?.catch(() => {
        });
      }
    };
  }
});

// src/codex-protocol/schema/ServerRequest.json
var ServerRequest_default;
var init_ServerRequest = __esm({
  "src/codex-protocol/schema/ServerRequest.json"() {
    ServerRequest_default = {
      $schema: "http://json-schema.org/draft-07/schema#",
      definitions: {
        AdditionalFileSystemPermissions: {
          properties: {
            entries: {
              items: {
                $ref: "#/definitions/FileSystemSandboxEntry"
              },
              type: [
                "array",
                "null"
              ]
            },
            globScanMaxDepth: {
              format: "uint",
              minimum: 1,
              type: [
                "integer",
                "null"
              ]
            },
            read: {
              description: "This will be removed in favor of `entries`.",
              items: {
                $ref: "#/definitions/LegacyAppPathString"
              },
              type: [
                "array",
                "null"
              ]
            },
            write: {
              description: "This will be removed in favor of `entries`.",
              items: {
                $ref: "#/definitions/LegacyAppPathString"
              },
              type: [
                "array",
                "null"
              ]
            }
          },
          type: "object"
        },
        AdditionalNetworkPermissions: {
          properties: {
            enabled: {
              type: [
                "boolean",
                "null"
              ]
            }
          },
          type: "object"
        },
        AdditionalPermissionProfile: {
          properties: {
            fileSystem: {
              anyOf: [
                {
                  $ref: "#/definitions/AdditionalFileSystemPermissions"
                },
                {
                  type: "null"
                }
              ]
            },
            network: {
              anyOf: [
                {
                  $ref: "#/definitions/AdditionalNetworkPermissions"
                },
                {
                  type: "null"
                }
              ],
              description: "Partial overlay used for per-command permission requests."
            }
          },
          type: "object"
        },
        ApplyPatchApprovalParams: {
          properties: {
            callId: {
              description: "Use to correlate this with [codex_protocol::protocol::PatchApplyBeginEvent] and [codex_protocol::protocol::PatchApplyEndEvent].",
              type: "string"
            },
            conversationId: {
              $ref: "#/definitions/ThreadId"
            },
            fileChanges: {
              additionalProperties: {
                $ref: "#/definitions/FileChange"
              },
              type: "object"
            },
            grantRoot: {
              description: "When set, the agent is asking the user to allow writes under this root for the remainder of the session (unclear if this is honored today).",
              type: [
                "string",
                "null"
              ]
            },
            reason: {
              description: "Optional explanatory reason (e.g. request for extra write access).",
              type: [
                "string",
                "null"
              ]
            }
          },
          required: [
            "callId",
            "conversationId",
            "fileChanges"
          ],
          type: "object"
        },
        AttestationGenerateParams: {
          type: "object"
        },
        ChatgptAuthTokensRefreshParams: {
          properties: {
            previousAccountId: {
              description: "Workspace/account identifier that Codex was previously using.\n\nClients that manage multiple accounts/workspaces can use this as a hint to refresh the token for the correct workspace.\n\nThis may be `null` when the prior auth state did not include a workspace identifier (`chatgpt_account_id`).",
              type: [
                "string",
                "null"
              ]
            },
            reason: {
              $ref: "#/definitions/ChatgptAuthTokensRefreshReason"
            }
          },
          required: [
            "reason"
          ],
          type: "object"
        },
        ChatgptAuthTokensRefreshReason: {
          oneOf: [
            {
              description: "Codex attempted a backend request and received `401 Unauthorized`.",
              enum: [
                "unauthorized"
              ],
              type: "string"
            }
          ]
        },
        CommandAction: {
          oneOf: [
            {
              properties: {
                command: {
                  type: "string"
                },
                name: {
                  type: "string"
                },
                path: {
                  $ref: "#/definitions/LegacyAppPathString"
                },
                type: {
                  enum: [
                    "read"
                  ],
                  title: "ReadCommandActionType",
                  type: "string"
                }
              },
              required: [
                "command",
                "name",
                "path",
                "type"
              ],
              title: "ReadCommandAction",
              type: "object"
            },
            {
              properties: {
                command: {
                  type: "string"
                },
                path: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "listFiles"
                  ],
                  title: "ListFilesCommandActionType",
                  type: "string"
                }
              },
              required: [
                "command",
                "type"
              ],
              title: "ListFilesCommandAction",
              type: "object"
            },
            {
              properties: {
                command: {
                  type: "string"
                },
                path: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                query: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "search"
                  ],
                  title: "SearchCommandActionType",
                  type: "string"
                }
              },
              required: [
                "command",
                "type"
              ],
              title: "SearchCommandAction",
              type: "object"
            },
            {
              properties: {
                command: {
                  type: "string"
                },
                type: {
                  enum: [
                    "unknown"
                  ],
                  title: "UnknownCommandActionType",
                  type: "string"
                }
              },
              required: [
                "command",
                "type"
              ],
              title: "UnknownCommandAction",
              type: "object"
            }
          ]
        },
        CommandExecutionApprovalDecision: {
          oneOf: [
            {
              description: "User approved the command.",
              enum: [
                "accept"
              ],
              type: "string"
            },
            {
              description: "User approved the command and future prompts in the same session-scoped approval cache should run without prompting.",
              enum: [
                "acceptForSession"
              ],
              type: "string"
            },
            {
              additionalProperties: false,
              description: "User approved the command, and wants to apply the proposed execpolicy amendment so future matching commands can run without prompting.",
              properties: {
                acceptWithExecpolicyAmendment: {
                  properties: {
                    execpolicy_amendment: {
                      items: {
                        type: "string"
                      },
                      type: "array"
                    }
                  },
                  required: [
                    "execpolicy_amendment"
                  ],
                  type: "object"
                }
              },
              required: [
                "acceptWithExecpolicyAmendment"
              ],
              title: "AcceptWithExecpolicyAmendmentCommandExecutionApprovalDecision",
              type: "object"
            },
            {
              additionalProperties: false,
              description: "User chose a persistent network policy rule (allow/deny) for this host.",
              properties: {
                applyNetworkPolicyAmendment: {
                  properties: {
                    network_policy_amendment: {
                      $ref: "#/definitions/NetworkPolicyAmendment"
                    }
                  },
                  required: [
                    "network_policy_amendment"
                  ],
                  type: "object"
                }
              },
              required: [
                "applyNetworkPolicyAmendment"
              ],
              title: "ApplyNetworkPolicyAmendmentCommandExecutionApprovalDecision",
              type: "object"
            },
            {
              description: "User denied the command. The agent will continue the turn.",
              enum: [
                "decline"
              ],
              type: "string"
            },
            {
              description: "User denied the command. The turn will also be immediately interrupted.",
              enum: [
                "cancel"
              ],
              type: "string"
            }
          ]
        },
        CommandExecutionApprovalKind: {
          description: "Distinguishes a command approval from input sent to an existing terminal.",
          enum: [
            "command",
            "writeStdin"
          ],
          type: "string"
        },
        CommandExecutionRequestApprovalParams: {
          properties: {
            approvalId: {
              description: "Unique identifier for this specific approval callback.\n\nFor regular shell/unified_exec approvals, this is null.\n\nFor zsh-exec-bridge subcommand approvals, multiple callbacks can belong to one parent `itemId`, so `approvalId` is a distinct opaque callback id (a UUID) used to disambiguate routing. Stdin approvals also use a distinct callback id; inspect `kind` to distinguish them.",
              type: [
                "string",
                "null"
              ]
            },
            command: {
              description: "The command to be executed.",
              type: [
                "string",
                "null"
              ]
            },
            commandActions: {
              description: "Best-effort parsed command actions for friendly display.",
              items: {
                $ref: "#/definitions/CommandAction"
              },
              type: [
                "array",
                "null"
              ]
            },
            cwd: {
              anyOf: [
                {
                  $ref: "#/definitions/LegacyAppPathString"
                },
                {
                  type: "null"
                }
              ],
              description: "The command's working directory."
            },
            environmentId: {
              default: null,
              description: "Environment in which the command will run.",
              type: [
                "string",
                "null"
              ]
            },
            itemId: {
              type: "string"
            },
            kind: {
              allOf: [
                {
                  $ref: "#/definitions/CommandExecutionApprovalKind"
                }
              ],
              default: "command",
              description: "Kind of action under review. Defaults to `command` for older servers."
            },
            networkApprovalContext: {
              anyOf: [
                {
                  $ref: "#/definitions/NetworkApprovalContext"
                },
                {
                  type: "null"
                }
              ],
              description: "Optional context for a managed-network approval prompt."
            },
            proposedExecpolicyAmendment: {
              description: "Optional proposed execpolicy amendment to allow similar commands without prompting.",
              items: {
                type: "string"
              },
              type: [
                "array",
                "null"
              ]
            },
            proposedNetworkPolicyAmendments: {
              description: "Optional proposed network policy amendments (allow/deny host) for future requests.",
              items: {
                $ref: "#/definitions/NetworkPolicyAmendment"
              },
              type: [
                "array",
                "null"
              ]
            },
            reason: {
              description: "Optional explanatory reason (e.g. request for network access).",
              type: [
                "string",
                "null"
              ]
            },
            startedAtMs: {
              description: "Unix timestamp (in milliseconds) when this approval request started.",
              format: "int64",
              type: "integer"
            },
            threadId: {
              type: "string"
            },
            turnId: {
              type: "string"
            }
          },
          required: [
            "itemId",
            "startedAtMs",
            "threadId",
            "turnId"
          ],
          type: "object"
        },
        DynamicToolCallParams: {
          properties: {
            arguments: true,
            callId: {
              type: "string"
            },
            namespace: {
              type: [
                "string",
                "null"
              ]
            },
            threadId: {
              type: "string"
            },
            tool: {
              type: "string"
            },
            turnId: {
              type: "string"
            }
          },
          required: [
            "arguments",
            "callId",
            "threadId",
            "tool",
            "turnId"
          ],
          type: "object"
        },
        ExecCommandApprovalParams: {
          properties: {
            approvalId: {
              description: "Identifier for this specific approval callback.",
              type: [
                "string",
                "null"
              ]
            },
            callId: {
              description: "Use to correlate this with [codex_protocol::protocol::ExecCommandBeginEvent] and [codex_protocol::protocol::ExecCommandEndEvent].",
              type: "string"
            },
            command: {
              items: {
                type: "string"
              },
              type: "array"
            },
            conversationId: {
              $ref: "#/definitions/ThreadId"
            },
            cwd: {
              type: "string"
            },
            parsedCmd: {
              items: {
                $ref: "#/definitions/ParsedCommand"
              },
              type: "array"
            },
            reason: {
              type: [
                "string",
                "null"
              ]
            }
          },
          required: [
            "callId",
            "command",
            "conversationId",
            "cwd",
            "parsedCmd"
          ],
          type: "object"
        },
        FileChange: {
          oneOf: [
            {
              properties: {
                content: {
                  type: "string"
                },
                type: {
                  enum: [
                    "add"
                  ],
                  title: "AddFileChangeType",
                  type: "string"
                }
              },
              required: [
                "content",
                "type"
              ],
              title: "AddFileChange",
              type: "object"
            },
            {
              properties: {
                content: {
                  type: "string"
                },
                type: {
                  enum: [
                    "delete"
                  ],
                  title: "DeleteFileChangeType",
                  type: "string"
                }
              },
              required: [
                "content",
                "type"
              ],
              title: "DeleteFileChange",
              type: "object"
            },
            {
              properties: {
                move_path: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "update"
                  ],
                  title: "UpdateFileChangeType",
                  type: "string"
                },
                unified_diff: {
                  type: "string"
                }
              },
              required: [
                "type",
                "unified_diff"
              ],
              title: "UpdateFileChange",
              type: "object"
            }
          ]
        },
        FileChangeRequestApprovalParams: {
          properties: {
            grantRoot: {
              description: "[UNSTABLE] When set, the agent is asking the user to allow writes under this root for the remainder of the session (unclear if this is honored today).",
              type: [
                "string",
                "null"
              ]
            },
            itemId: {
              type: "string"
            },
            reason: {
              description: "Optional explanatory reason (e.g. request for extra write access).",
              type: [
                "string",
                "null"
              ]
            },
            startedAtMs: {
              description: "Unix timestamp (in milliseconds) when this approval request started.",
              format: "int64",
              type: "integer"
            },
            threadId: {
              type: "string"
            },
            turnId: {
              type: "string"
            }
          },
          required: [
            "itemId",
            "startedAtMs",
            "threadId",
            "turnId"
          ],
          type: "object"
        },
        FileSystemAccessMode: {
          enum: [
            "read",
            "write",
            "deny"
          ],
          type: "string"
        },
        FileSystemPath: {
          oneOf: [
            {
              properties: {
                path: {
                  $ref: "#/definitions/LegacyAppPathString"
                },
                type: {
                  enum: [
                    "path"
                  ],
                  title: "PathFileSystemPathType",
                  type: "string"
                }
              },
              required: [
                "path",
                "type"
              ],
              title: "PathFileSystemPath",
              type: "object"
            },
            {
              properties: {
                pattern: {
                  type: "string"
                },
                type: {
                  enum: [
                    "glob_pattern"
                  ],
                  title: "GlobPatternFileSystemPathType",
                  type: "string"
                }
              },
              required: [
                "pattern",
                "type"
              ],
              title: "GlobPatternFileSystemPath",
              type: "object"
            },
            {
              properties: {
                type: {
                  enum: [
                    "special"
                  ],
                  title: "SpecialFileSystemPathType",
                  type: "string"
                },
                value: {
                  $ref: "#/definitions/FileSystemSpecialPath"
                }
              },
              required: [
                "type",
                "value"
              ],
              title: "SpecialFileSystemPath",
              type: "object"
            }
          ]
        },
        FileSystemSandboxEntry: {
          properties: {
            access: {
              $ref: "#/definitions/FileSystemAccessMode"
            },
            path: {
              $ref: "#/definitions/FileSystemPath"
            }
          },
          required: [
            "access",
            "path"
          ],
          type: "object"
        },
        FileSystemSpecialPath: {
          oneOf: [
            {
              properties: {
                kind: {
                  enum: [
                    "root"
                  ],
                  type: "string"
                }
              },
              required: [
                "kind"
              ],
              title: "RootFileSystemSpecialPath",
              type: "object"
            },
            {
              properties: {
                kind: {
                  enum: [
                    "minimal"
                  ],
                  type: "string"
                }
              },
              required: [
                "kind"
              ],
              title: "MinimalFileSystemSpecialPath",
              type: "object"
            },
            {
              properties: {
                kind: {
                  enum: [
                    "project_roots"
                  ],
                  type: "string"
                },
                subpath: {
                  anyOf: [
                    {
                      $ref: "#/definitions/LegacyAppPathString"
                    },
                    {
                      type: "null"
                    }
                  ]
                }
              },
              required: [
                "kind"
              ],
              title: "KindFileSystemSpecialPath",
              type: "object"
            },
            {
              properties: {
                kind: {
                  enum: [
                    "tmpdir"
                  ],
                  type: "string"
                }
              },
              required: [
                "kind"
              ],
              title: "TmpdirFileSystemSpecialPath",
              type: "object"
            },
            {
              properties: {
                kind: {
                  enum: [
                    "slash_tmp"
                  ],
                  type: "string"
                }
              },
              required: [
                "kind"
              ],
              title: "SlashTmpFileSystemSpecialPath",
              type: "object"
            },
            {
              properties: {
                kind: {
                  enum: [
                    "unknown"
                  ],
                  type: "string"
                },
                path: {
                  type: "string"
                },
                subpath: {
                  anyOf: [
                    {
                      $ref: "#/definitions/LegacyAppPathString"
                    },
                    {
                      type: "null"
                    }
                  ]
                }
              },
              required: [
                "kind",
                "path"
              ],
              type: "object"
            }
          ]
        },
        LegacyAppPathString: {
          type: "string"
        },
        McpElicitationArrayType: {
          enum: [
            "array"
          ],
          type: "string"
        },
        McpElicitationBooleanSchema: {
          additionalProperties: false,
          properties: {
            default: {
              type: [
                "boolean",
                "null"
              ]
            },
            description: {
              type: [
                "string",
                "null"
              ]
            },
            title: {
              type: [
                "string",
                "null"
              ]
            },
            type: {
              $ref: "#/definitions/McpElicitationBooleanType"
            }
          },
          required: [
            "type"
          ],
          type: "object"
        },
        McpElicitationBooleanType: {
          enum: [
            "boolean"
          ],
          type: "string"
        },
        McpElicitationConstOption: {
          additionalProperties: false,
          properties: {
            const: {
              type: "string"
            },
            title: {
              type: "string"
            }
          },
          required: [
            "const",
            "title"
          ],
          type: "object"
        },
        McpElicitationEnumSchema: {
          anyOf: [
            {
              $ref: "#/definitions/McpElicitationSingleSelectEnumSchema"
            },
            {
              $ref: "#/definitions/McpElicitationMultiSelectEnumSchema"
            },
            {
              $ref: "#/definitions/McpElicitationLegacyTitledEnumSchema"
            }
          ]
        },
        McpElicitationLegacyTitledEnumSchema: {
          additionalProperties: false,
          properties: {
            default: {
              type: [
                "string",
                "null"
              ]
            },
            description: {
              type: [
                "string",
                "null"
              ]
            },
            enum: {
              items: {
                type: "string"
              },
              type: "array"
            },
            enumNames: {
              items: {
                type: "string"
              },
              type: [
                "array",
                "null"
              ]
            },
            title: {
              type: [
                "string",
                "null"
              ]
            },
            type: {
              $ref: "#/definitions/McpElicitationStringType"
            }
          },
          required: [
            "enum",
            "type"
          ],
          type: "object"
        },
        McpElicitationMultiSelectEnumSchema: {
          anyOf: [
            {
              $ref: "#/definitions/McpElicitationUntitledMultiSelectEnumSchema"
            },
            {
              $ref: "#/definitions/McpElicitationTitledMultiSelectEnumSchema"
            }
          ]
        },
        McpElicitationNumberSchema: {
          additionalProperties: false,
          properties: {
            default: {
              format: "double",
              type: [
                "number",
                "null"
              ]
            },
            description: {
              type: [
                "string",
                "null"
              ]
            },
            maximum: {
              format: "double",
              type: [
                "number",
                "null"
              ]
            },
            minimum: {
              format: "double",
              type: [
                "number",
                "null"
              ]
            },
            title: {
              type: [
                "string",
                "null"
              ]
            },
            type: {
              $ref: "#/definitions/McpElicitationNumberType"
            }
          },
          required: [
            "type"
          ],
          type: "object"
        },
        McpElicitationNumberType: {
          enum: [
            "number",
            "integer"
          ],
          type: "string"
        },
        McpElicitationObjectType: {
          enum: [
            "object"
          ],
          type: "string"
        },
        McpElicitationPrimitiveSchema: {
          anyOf: [
            {
              $ref: "#/definitions/McpElicitationEnumSchema"
            },
            {
              $ref: "#/definitions/McpElicitationStringSchema"
            },
            {
              $ref: "#/definitions/McpElicitationNumberSchema"
            },
            {
              $ref: "#/definitions/McpElicitationBooleanSchema"
            }
          ]
        },
        McpElicitationSchema: {
          additionalProperties: false,
          description: "Typed form schema for MCP `elicitation/create` requests.\n\nThis matches the `requestedSchema` shape from the MCP 2025-11-25 `ElicitRequestFormParams` schema.",
          properties: {
            $schema: {
              type: [
                "string",
                "null"
              ]
            },
            properties: {
              additionalProperties: {
                $ref: "#/definitions/McpElicitationPrimitiveSchema"
              },
              type: "object"
            },
            required: {
              items: {
                type: "string"
              },
              type: [
                "array",
                "null"
              ]
            },
            type: {
              $ref: "#/definitions/McpElicitationObjectType"
            }
          },
          required: [
            "properties",
            "type"
          ],
          type: "object"
        },
        McpElicitationSingleSelectEnumSchema: {
          anyOf: [
            {
              $ref: "#/definitions/McpElicitationUntitledSingleSelectEnumSchema"
            },
            {
              $ref: "#/definitions/McpElicitationTitledSingleSelectEnumSchema"
            }
          ]
        },
        McpElicitationStringFormat: {
          enum: [
            "email",
            "uri",
            "date",
            "date-time"
          ],
          type: "string"
        },
        McpElicitationStringSchema: {
          additionalProperties: false,
          properties: {
            default: {
              type: [
                "string",
                "null"
              ]
            },
            description: {
              type: [
                "string",
                "null"
              ]
            },
            format: {
              anyOf: [
                {
                  $ref: "#/definitions/McpElicitationStringFormat"
                },
                {
                  type: "null"
                }
              ]
            },
            maxLength: {
              format: "uint32",
              minimum: 0,
              type: [
                "integer",
                "null"
              ]
            },
            minLength: {
              format: "uint32",
              minimum: 0,
              type: [
                "integer",
                "null"
              ]
            },
            title: {
              type: [
                "string",
                "null"
              ]
            },
            type: {
              $ref: "#/definitions/McpElicitationStringType"
            }
          },
          required: [
            "type"
          ],
          type: "object"
        },
        McpElicitationStringType: {
          enum: [
            "string"
          ],
          type: "string"
        },
        McpElicitationTitledEnumItems: {
          additionalProperties: false,
          properties: {
            anyOf: {
              items: {
                $ref: "#/definitions/McpElicitationConstOption"
              },
              type: "array"
            }
          },
          required: [
            "anyOf"
          ],
          type: "object"
        },
        McpElicitationTitledMultiSelectEnumSchema: {
          additionalProperties: false,
          properties: {
            default: {
              items: {
                type: "string"
              },
              type: [
                "array",
                "null"
              ]
            },
            description: {
              type: [
                "string",
                "null"
              ]
            },
            items: {
              $ref: "#/definitions/McpElicitationTitledEnumItems"
            },
            maxItems: {
              format: "uint64",
              minimum: 0,
              type: [
                "integer",
                "null"
              ]
            },
            minItems: {
              format: "uint64",
              minimum: 0,
              type: [
                "integer",
                "null"
              ]
            },
            title: {
              type: [
                "string",
                "null"
              ]
            },
            type: {
              $ref: "#/definitions/McpElicitationArrayType"
            }
          },
          required: [
            "items",
            "type"
          ],
          type: "object"
        },
        McpElicitationTitledSingleSelectEnumSchema: {
          additionalProperties: false,
          properties: {
            default: {
              type: [
                "string",
                "null"
              ]
            },
            description: {
              type: [
                "string",
                "null"
              ]
            },
            oneOf: {
              items: {
                $ref: "#/definitions/McpElicitationConstOption"
              },
              type: "array"
            },
            title: {
              type: [
                "string",
                "null"
              ]
            },
            type: {
              $ref: "#/definitions/McpElicitationStringType"
            }
          },
          required: [
            "oneOf",
            "type"
          ],
          type: "object"
        },
        McpElicitationUntitledEnumItems: {
          additionalProperties: false,
          properties: {
            enum: {
              items: {
                type: "string"
              },
              type: "array"
            },
            type: {
              $ref: "#/definitions/McpElicitationStringType"
            }
          },
          required: [
            "enum",
            "type"
          ],
          type: "object"
        },
        McpElicitationUntitledMultiSelectEnumSchema: {
          additionalProperties: false,
          properties: {
            default: {
              items: {
                type: "string"
              },
              type: [
                "array",
                "null"
              ]
            },
            description: {
              type: [
                "string",
                "null"
              ]
            },
            items: {
              $ref: "#/definitions/McpElicitationUntitledEnumItems"
            },
            maxItems: {
              format: "uint64",
              minimum: 0,
              type: [
                "integer",
                "null"
              ]
            },
            minItems: {
              format: "uint64",
              minimum: 0,
              type: [
                "integer",
                "null"
              ]
            },
            title: {
              type: [
                "string",
                "null"
              ]
            },
            type: {
              $ref: "#/definitions/McpElicitationArrayType"
            }
          },
          required: [
            "items",
            "type"
          ],
          type: "object"
        },
        McpElicitationUntitledSingleSelectEnumSchema: {
          additionalProperties: false,
          properties: {
            default: {
              type: [
                "string",
                "null"
              ]
            },
            description: {
              type: [
                "string",
                "null"
              ]
            },
            enum: {
              items: {
                type: "string"
              },
              type: "array"
            },
            title: {
              type: [
                "string",
                "null"
              ]
            },
            type: {
              $ref: "#/definitions/McpElicitationStringType"
            }
          },
          required: [
            "enum",
            "type"
          ],
          type: "object"
        },
        McpServerElicitationRequestParams: {
          oneOf: [
            {
              properties: {
                _meta: true,
                message: {
                  type: "string"
                },
                mode: {
                  enum: [
                    "form"
                  ],
                  type: "string"
                },
                requestedSchema: {
                  $ref: "#/definitions/McpElicitationSchema"
                }
              },
              required: [
                "message",
                "mode",
                "requestedSchema"
              ],
              type: "object"
            },
            {
              properties: {
                _meta: true,
                message: {
                  type: "string"
                },
                mode: {
                  enum: [
                    "openai/form"
                  ],
                  type: "string"
                },
                requestedSchema: true
              },
              required: [
                "message",
                "mode",
                "requestedSchema"
              ],
              type: "object"
            },
            {
              properties: {
                _meta: true,
                message: {
                  type: "string"
                },
                mode: {
                  enum: [
                    "openaiForm"
                  ],
                  type: "string"
                },
                requestedSchema: true
              },
              required: [
                "message",
                "mode",
                "requestedSchema"
              ],
              type: "object"
            },
            {
              properties: {
                _meta: true,
                elicitationId: {
                  type: "string"
                },
                message: {
                  type: "string"
                },
                mode: {
                  enum: [
                    "url"
                  ],
                  type: "string"
                },
                url: {
                  type: "string"
                }
              },
              required: [
                "elicitationId",
                "message",
                "mode",
                "url"
              ],
              type: "object"
            }
          ],
          properties: {
            serverName: {
              type: "string"
            },
            threadId: {
              type: "string"
            },
            turnId: {
              description: "Active Codex turn when this elicitation was observed, if app-server could correlate one.\n\nThis is nullable because MCP models elicitation as a standalone server-to-client request identified by the MCP server request id. It may be triggered during a turn, but turn context is app-server correlation rather than part of the protocol identity of the elicitation itself.",
              type: [
                "string",
                "null"
              ]
            }
          },
          required: [
            "serverName",
            "threadId"
          ],
          type: "object"
        },
        NetworkApprovalContext: {
          properties: {
            host: {
              type: "string"
            },
            protocol: {
              $ref: "#/definitions/NetworkApprovalProtocol"
            }
          },
          required: [
            "host",
            "protocol"
          ],
          type: "object"
        },
        NetworkApprovalProtocol: {
          enum: [
            "http",
            "https",
            "socks5Tcp",
            "socks5Udp"
          ],
          type: "string"
        },
        NetworkPolicyAmendment: {
          properties: {
            action: {
              $ref: "#/definitions/NetworkPolicyRuleAction"
            },
            host: {
              type: "string"
            }
          },
          required: [
            "action",
            "host"
          ],
          type: "object"
        },
        NetworkPolicyRuleAction: {
          enum: [
            "allow",
            "deny"
          ],
          type: "string"
        },
        ParsedCommand: {
          oneOf: [
            {
              properties: {
                cmd: {
                  type: "string"
                },
                name: {
                  type: "string"
                },
                path: {
                  description: "(Best effort) Path to the file being read by the command. When possible, this is an absolute path, though when relative, it should be resolved against the `cwd`` that will be used to run the command to derive the absolute path.",
                  type: "string"
                },
                type: {
                  enum: [
                    "read"
                  ],
                  title: "ReadParsedCommandType",
                  type: "string"
                }
              },
              required: [
                "cmd",
                "name",
                "path",
                "type"
              ],
              title: "ReadParsedCommand",
              type: "object"
            },
            {
              properties: {
                cmd: {
                  type: "string"
                },
                path: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "list_files"
                  ],
                  title: "ListFilesParsedCommandType",
                  type: "string"
                }
              },
              required: [
                "cmd",
                "type"
              ],
              title: "ListFilesParsedCommand",
              type: "object"
            },
            {
              properties: {
                cmd: {
                  type: "string"
                },
                path: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                query: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "search"
                  ],
                  title: "SearchParsedCommandType",
                  type: "string"
                }
              },
              required: [
                "cmd",
                "type"
              ],
              title: "SearchParsedCommand",
              type: "object"
            },
            {
              properties: {
                cmd: {
                  type: "string"
                },
                type: {
                  enum: [
                    "unknown"
                  ],
                  title: "UnknownParsedCommandType",
                  type: "string"
                }
              },
              required: [
                "cmd",
                "type"
              ],
              title: "UnknownParsedCommand",
              type: "object"
            }
          ]
        },
        PermissionsRequestApprovalParams: {
          properties: {
            cwd: {
              $ref: "#/definitions/LegacyAppPathString"
            },
            environmentId: {
              default: null,
              type: [
                "string",
                "null"
              ]
            },
            itemId: {
              type: "string"
            },
            permissions: {
              $ref: "#/definitions/RequestPermissionProfile"
            },
            reason: {
              type: [
                "string",
                "null"
              ]
            },
            startedAtMs: {
              description: "Unix timestamp (in milliseconds) when this approval request started.",
              format: "int64",
              type: "integer"
            },
            threadId: {
              type: "string"
            },
            turnId: {
              type: "string"
            }
          },
          required: [
            "cwd",
            "itemId",
            "permissions",
            "startedAtMs",
            "threadId",
            "turnId"
          ],
          type: "object"
        },
        RequestId: {
          anyOf: [
            {
              type: "string"
            },
            {
              format: "int64",
              type: "integer"
            }
          ]
        },
        RequestPermissionProfile: {
          additionalProperties: false,
          properties: {
            fileSystem: {
              anyOf: [
                {
                  $ref: "#/definitions/AdditionalFileSystemPermissions"
                },
                {
                  type: "null"
                }
              ]
            },
            network: {
              anyOf: [
                {
                  $ref: "#/definitions/AdditionalNetworkPermissions"
                },
                {
                  type: "null"
                }
              ]
            }
          },
          type: "object"
        },
        ThreadId: {
          type: "string"
        },
        ToolRequestUserInputOption: {
          description: "EXPERIMENTAL. Defines a single selectable option for request_user_input.",
          properties: {
            description: {
              type: "string"
            },
            label: {
              type: "string"
            }
          },
          required: [
            "description",
            "label"
          ],
          type: "object"
        },
        ToolRequestUserInputParams: {
          description: "EXPERIMENTAL. Params sent with a request_user_input event.",
          properties: {
            autoResolutionMs: {
              default: null,
              description: "@deprecated Use `isBlocking` to decide whether the request should block.",
              format: "uint64",
              minimum: 0,
              type: [
                "integer",
                "null"
              ]
            },
            isBlocking: {
              type: "boolean"
            },
            itemId: {
              type: "string"
            },
            questions: {
              items: {
                $ref: "#/definitions/ToolRequestUserInputQuestion"
              },
              type: "array"
            },
            threadId: {
              type: "string"
            },
            turnId: {
              type: "string"
            }
          },
          required: [
            "isBlocking",
            "itemId",
            "questions",
            "threadId",
            "turnId"
          ],
          type: "object"
        },
        ToolRequestUserInputQuestion: {
          description: "EXPERIMENTAL. Represents one request_user_input question and its required options.",
          properties: {
            header: {
              type: "string"
            },
            id: {
              type: "string"
            },
            isOther: {
              default: false,
              type: "boolean"
            },
            isSecret: {
              default: false,
              type: "boolean"
            },
            options: {
              items: {
                $ref: "#/definitions/ToolRequestUserInputOption"
              },
              type: [
                "array",
                "null"
              ]
            },
            question: {
              type: "string"
            }
          },
          required: [
            "header",
            "id",
            "question"
          ],
          type: "object"
        }
      },
      description: "Request initiated from the server and sent to the client.",
      oneOf: [
        {
          description: "NEW APIs Sent when approval is requested for a specific command execution. This request is used for Turns started via turn/start.",
          properties: {
            id: {
              $ref: "#/definitions/RequestId"
            },
            method: {
              enum: [
                "item/commandExecution/requestApproval"
              ],
              title: "Item/commandExecution/requestApprovalRequestMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/CommandExecutionRequestApprovalParams"
            }
          },
          required: [
            "id",
            "method",
            "params"
          ],
          title: "Item/commandExecution/requestApprovalRequest",
          type: "object"
        },
        {
          description: "Sent when approval is requested for a specific file change. This request is used for Turns started via turn/start.",
          properties: {
            id: {
              $ref: "#/definitions/RequestId"
            },
            method: {
              enum: [
                "item/fileChange/requestApproval"
              ],
              title: "Item/fileChange/requestApprovalRequestMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/FileChangeRequestApprovalParams"
            }
          },
          required: [
            "id",
            "method",
            "params"
          ],
          title: "Item/fileChange/requestApprovalRequest",
          type: "object"
        },
        {
          description: "EXPERIMENTAL - Request input from the user for a tool call.",
          properties: {
            id: {
              $ref: "#/definitions/RequestId"
            },
            method: {
              enum: [
                "item/tool/requestUserInput"
              ],
              title: "Item/tool/requestUserInputRequestMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ToolRequestUserInputParams"
            }
          },
          required: [
            "id",
            "method",
            "params"
          ],
          title: "Item/tool/requestUserInputRequest",
          type: "object"
        },
        {
          description: "Request input for an MCP server elicitation.",
          properties: {
            id: {
              $ref: "#/definitions/RequestId"
            },
            method: {
              enum: [
                "mcpServer/elicitation/request"
              ],
              title: "McpServer/elicitation/requestRequestMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/McpServerElicitationRequestParams"
            }
          },
          required: [
            "id",
            "method",
            "params"
          ],
          title: "McpServer/elicitation/requestRequest",
          type: "object"
        },
        {
          description: "Request approval for additional permissions from the user.",
          properties: {
            id: {
              $ref: "#/definitions/RequestId"
            },
            method: {
              enum: [
                "item/permissions/requestApproval"
              ],
              title: "Item/permissions/requestApprovalRequestMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/PermissionsRequestApprovalParams"
            }
          },
          required: [
            "id",
            "method",
            "params"
          ],
          title: "Item/permissions/requestApprovalRequest",
          type: "object"
        },
        {
          description: "Execute a dynamic tool call on the client.",
          properties: {
            id: {
              $ref: "#/definitions/RequestId"
            },
            method: {
              enum: [
                "item/tool/call"
              ],
              title: "Item/tool/callRequestMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/DynamicToolCallParams"
            }
          },
          required: [
            "id",
            "method",
            "params"
          ],
          title: "Item/tool/callRequest",
          type: "object"
        },
        {
          properties: {
            id: {
              $ref: "#/definitions/RequestId"
            },
            method: {
              enum: [
                "account/chatgptAuthTokens/refresh"
              ],
              title: "Account/chatgptAuthTokens/refreshRequestMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ChatgptAuthTokensRefreshParams"
            }
          },
          required: [
            "id",
            "method",
            "params"
          ],
          title: "Account/chatgptAuthTokens/refreshRequest",
          type: "object"
        },
        {
          description: "Generate a fresh upstream attestation result on demand.",
          properties: {
            id: {
              $ref: "#/definitions/RequestId"
            },
            method: {
              enum: [
                "attestation/generate"
              ],
              title: "Attestation/generateRequestMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/AttestationGenerateParams"
            }
          },
          required: [
            "id",
            "method",
            "params"
          ],
          title: "Attestation/generateRequest",
          type: "object"
        },
        {
          description: "DEPRECATED APIs below Request to approve a patch. This request is used for Turns started via the legacy APIs (i.e. SendUserTurn, SendUserMessage).",
          properties: {
            id: {
              $ref: "#/definitions/RequestId"
            },
            method: {
              enum: [
                "applyPatchApproval"
              ],
              title: "ApplyPatchApprovalRequestMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ApplyPatchApprovalParams"
            }
          },
          required: [
            "id",
            "method",
            "params"
          ],
          title: "ApplyPatchApprovalRequest",
          type: "object"
        },
        {
          description: "Request to exec a command. This request is used for Turns started via the legacy APIs (i.e. SendUserTurn, SendUserMessage).",
          properties: {
            id: {
              $ref: "#/definitions/RequestId"
            },
            method: {
              enum: [
                "execCommandApproval"
              ],
              title: "ExecCommandApprovalRequestMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ExecCommandApprovalParams"
            }
          },
          required: [
            "id",
            "method",
            "params"
          ],
          title: "ExecCommandApprovalRequest",
          type: "object"
        }
      ],
      title: "ServerRequest"
    };
  }
});

// src/codex-protocol/schema/ServerNotification.json
var ServerNotification_default;
var init_ServerNotification = __esm({
  "src/codex-protocol/schema/ServerNotification.json"() {
    ServerNotification_default = {
      $schema: "http://json-schema.org/draft-07/schema#",
      definitions: {
        AbsolutePathBuf: {
          description: "A path that is guaranteed to be absolute and normalized (though it is not guaranteed to be canonicalized or exist on the filesystem).\n\nIMPORTANT: When deserializing an `AbsolutePathBuf`, a base path must be set using [AbsolutePathBufGuard::new]. If no base path is set, the deserialization will fail unless the path being deserialized is already absolute.",
          type: "string"
        },
        AccountLoginCompletedNotification: {
          properties: {
            error: {
              type: [
                "string",
                "null"
              ]
            },
            loginId: {
              type: [
                "string",
                "null"
              ]
            },
            onboardingEntrypoint: {
              anyOf: [
                {
                  $ref: "#/definitions/DesktopOnboardingEntrypoint"
                },
                {
                  type: "null"
                }
              ]
            },
            success: {
              type: "boolean"
            }
          },
          required: [
            "success"
          ],
          type: "object"
        },
        AccountRateLimitsUpdatedNotification: {
          description: "Sparse rolling rate-limit update.\n\nClients should merge available values into the most recent `account/rateLimits/read` response or refetch that snapshot. Nullable account metadata may be unavailable in a rolling update and does not clear a previously observed value.",
          properties: {
            rateLimits: {
              $ref: "#/definitions/RateLimitSnapshot"
            }
          },
          required: [
            "rateLimits"
          ],
          type: "object"
        },
        AccountUpdatedNotification: {
          properties: {
            authMode: {
              anyOf: [
                {
                  $ref: "#/definitions/AuthMode"
                },
                {
                  type: "null"
                }
              ]
            },
            planType: {
              anyOf: [
                {
                  $ref: "#/definitions/PlanType"
                },
                {
                  type: "null"
                }
              ]
            }
          },
          type: "object"
        },
        ActivePermissionProfile: {
          properties: {
            extends: {
              default: null,
              description: "Parent profile identifier from the selected permissions profile's `extends` setting, when present.",
              type: [
                "string",
                "null"
              ]
            },
            id: {
              description: "Identifier from `default_permissions` or the implicit built-in default, such as `:workspace` or a user-defined `[permissions.<id>]` profile.",
              type: "string"
            }
          },
          required: [
            "id"
          ],
          type: "object"
        },
        AdditionalFileSystemPermissions: {
          properties: {
            entries: {
              items: {
                $ref: "#/definitions/FileSystemSandboxEntry"
              },
              type: [
                "array",
                "null"
              ]
            },
            globScanMaxDepth: {
              format: "uint",
              minimum: 1,
              type: [
                "integer",
                "null"
              ]
            },
            read: {
              description: "This will be removed in favor of `entries`.",
              items: {
                $ref: "#/definitions/LegacyAppPathString"
              },
              type: [
                "array",
                "null"
              ]
            },
            write: {
              description: "This will be removed in favor of `entries`.",
              items: {
                $ref: "#/definitions/LegacyAppPathString"
              },
              type: [
                "array",
                "null"
              ]
            }
          },
          type: "object"
        },
        AdditionalNetworkPermissions: {
          properties: {
            enabled: {
              type: [
                "boolean",
                "null"
              ]
            }
          },
          type: "object"
        },
        AgentMessageDelivery: {
          enum: [
            "async"
          ],
          type: "string"
        },
        AgentMessageDeltaNotification: {
          properties: {
            delta: {
              type: "string"
            },
            itemId: {
              type: "string"
            },
            threadId: {
              type: "string"
            },
            turnId: {
              type: "string"
            }
          },
          required: [
            "delta",
            "itemId",
            "threadId",
            "turnId"
          ],
          type: "object"
        },
        AgentPath: {
          type: "string"
        },
        AppBranding: {
          description: "EXPERIMENTAL - app metadata returned by app-list APIs.",
          properties: {
            category: {
              type: [
                "string",
                "null"
              ]
            },
            developer: {
              type: [
                "string",
                "null"
              ]
            },
            isDiscoverableApp: {
              type: "boolean"
            },
            privacyPolicy: {
              type: [
                "string",
                "null"
              ]
            },
            termsOfService: {
              type: [
                "string",
                "null"
              ]
            },
            website: {
              type: [
                "string",
                "null"
              ]
            }
          },
          required: [
            "isDiscoverableApp"
          ],
          type: "object"
        },
        AppInfo: {
          description: "EXPERIMENTAL - app metadata returned by app-list APIs.",
          properties: {
            appMetadata: {
              anyOf: [
                {
                  $ref: "#/definitions/AppMetadata"
                },
                {
                  type: "null"
                }
              ]
            },
            branding: {
              anyOf: [
                {
                  $ref: "#/definitions/AppBranding"
                },
                {
                  type: "null"
                }
              ]
            },
            description: {
              type: [
                "string",
                "null"
              ]
            },
            distributionChannel: {
              type: [
                "string",
                "null"
              ]
            },
            iconAssets: {
              additionalProperties: {
                type: "string"
              },
              type: [
                "object",
                "null"
              ]
            },
            iconDarkAssets: {
              additionalProperties: {
                type: "string"
              },
              type: [
                "object",
                "null"
              ]
            },
            id: {
              type: "string"
            },
            installUrl: {
              type: [
                "string",
                "null"
              ]
            },
            isAccessible: {
              default: false,
              type: "boolean"
            },
            isEnabled: {
              default: true,
              description: "Whether this app is enabled in config.toml. Example: ```toml [apps.bad_app] enabled = false ```",
              type: "boolean"
            },
            labels: {
              additionalProperties: {
                type: "string"
              },
              type: [
                "object",
                "null"
              ]
            },
            logoUrl: {
              type: [
                "string",
                "null"
              ]
            },
            logoUrlDark: {
              type: [
                "string",
                "null"
              ]
            },
            name: {
              type: "string"
            },
            pluginDisplayNames: {
              default: [],
              items: {
                type: "string"
              },
              type: "array"
            }
          },
          required: [
            "id",
            "name"
          ],
          type: "object"
        },
        AppListUpdatedNotification: {
          description: "EXPERIMENTAL - notification emitted when the app list changes.",
          properties: {
            data: {
              items: {
                $ref: "#/definitions/AppInfo"
              },
              type: "array"
            }
          },
          required: [
            "data"
          ],
          type: "object"
        },
        AppMetadata: {
          properties: {
            categories: {
              items: {
                type: "string"
              },
              type: [
                "array",
                "null"
              ]
            },
            developer: {
              type: [
                "string",
                "null"
              ]
            },
            firstPartyRequiresInstall: {
              type: [
                "boolean",
                "null"
              ]
            },
            review: {
              anyOf: [
                {
                  $ref: "#/definitions/AppReview"
                },
                {
                  type: "null"
                }
              ]
            },
            screenshots: {
              items: {
                $ref: "#/definitions/AppScreenshot"
              },
              type: [
                "array",
                "null"
              ]
            },
            seoDescription: {
              type: [
                "string",
                "null"
              ]
            },
            showInComposerWhenUnlinked: {
              type: [
                "boolean",
                "null"
              ]
            },
            subCategories: {
              items: {
                type: "string"
              },
              type: [
                "array",
                "null"
              ]
            },
            version: {
              type: [
                "string",
                "null"
              ]
            },
            versionId: {
              type: [
                "string",
                "null"
              ]
            },
            versionNotes: {
              type: [
                "string",
                "null"
              ]
            }
          },
          type: "object"
        },
        AppReview: {
          properties: {
            status: {
              type: "string"
            }
          },
          required: [
            "status"
          ],
          type: "object"
        },
        AppScreenshot: {
          properties: {
            fileId: {
              type: [
                "string",
                "null"
              ]
            },
            url: {
              type: [
                "string",
                "null"
              ]
            },
            userPrompt: {
              type: "string"
            }
          },
          required: [
            "userPrompt"
          ],
          type: "object"
        },
        ApprovalsReviewer: {
          description: "Configures who approval requests are routed to for review. Examples include sandbox escapes, blocked network access, MCP approval prompts, and ARC escalations. Defaults to `user`. `auto_review` uses a carefully prompted subagent to gather relevant context and apply a risk-based decision framework before approving or denying the request. The legacy value `guardian_subagent` is accepted for compatibility.",
          enum: [
            "user",
            "auto_review",
            "guardian_subagent"
          ],
          type: "string"
        },
        AskForApproval: {
          oneOf: [
            {
              enum: [
                "untrusted",
                "on-request",
                "never"
              ],
              type: "string"
            },
            {
              additionalProperties: false,
              properties: {
                granular: {
                  properties: {
                    mcp_elicitations: {
                      type: "boolean"
                    },
                    request_permissions: {
                      default: false,
                      type: "boolean"
                    },
                    rules: {
                      type: "boolean"
                    },
                    sandbox_approval: {
                      type: "boolean"
                    },
                    skill_approval: {
                      default: false,
                      type: "boolean"
                    }
                  },
                  required: [
                    "mcp_elicitations",
                    "rules",
                    "sandbox_approval"
                  ],
                  type: "object"
                }
              },
              required: [
                "granular"
              ],
              title: "GranularAskForApproval",
              type: "object"
            }
          ]
        },
        AsyncUserInputQuestion: {
          additionalProperties: false,
          properties: {
            options: {
              items: {
                type: "string"
              },
              type: [
                "array",
                "null"
              ]
            },
            title: {
              type: "string"
            }
          },
          required: [
            "title"
          ],
          type: "object"
        },
        AuthMode: {
          description: "Authentication mode for OpenAI-backed providers.",
          oneOf: [
            {
              description: "OpenAI API key provided by the caller and stored by Codex.",
              enum: [
                "apikey"
              ],
              type: "string"
            },
            {
              description: "ChatGPT OAuth managed by Codex (tokens persisted and refreshed by Codex).",
              enum: [
                "chatgpt"
              ],
              type: "string"
            },
            {
              description: "[UNSTABLE] FOR OPENAI INTERNAL USE ONLY - DO NOT USE.\n\nChatGPT auth tokens are supplied by an external host app and are only stored in memory. Token refresh must be handled by the external host app.",
              enum: [
                "chatgptAuthTokens"
              ],
              type: "string"
            },
            {
              description: "Backend auth supplied as request headers.",
              enum: [
                "headers"
              ],
              type: "string"
            },
            {
              description: "Programmatic Codex auth backed by a registered Agent Identity.",
              enum: [
                "agentIdentity"
              ],
              type: "string"
            },
            {
              description: "Programmatic Codex auth backed by a personal access token.",
              enum: [
                "personalAccessToken"
              ],
              type: "string"
            },
            {
              description: "Amazon Bedrock bearer token managed by Codex.",
              enum: [
                "bedrockApiKey"
              ],
              type: "string"
            },
            {
              description: "Amazon Bedrock AWS access keys managed by Codex.",
              enum: [
                "bedrockAccessKeys"
              ],
              type: "string"
            }
          ]
        },
        AuthRecoveryNotification: {
          properties: {
            message: {
              type: "string"
            },
            provider: {
              type: "string"
            },
            threadId: {
              type: "string"
            },
            turnId: {
              type: "string"
            }
          },
          required: [
            "message",
            "provider",
            "threadId",
            "turnId"
          ],
          type: "object"
        },
        AutoReviewDecisionSource: {
          description: "[UNSTABLE] Source that produced a terminal approval auto-review decision.",
          enum: [
            "agent"
          ],
          type: "string"
        },
        ByteRange: {
          properties: {
            end: {
              format: "uint",
              minimum: 0,
              type: "integer"
            },
            start: {
              format: "uint",
              minimum: 0,
              type: "integer"
            }
          },
          required: [
            "end",
            "start"
          ],
          type: "object"
        },
        CodexErrorInfo: {
          anyOf: [
            {
              enum: [
                "contextWindowExceeded",
                "sessionBudgetExceeded",
                "usageLimitExceeded",
                "rateLimitExceeded",
                "flexUnavailable",
                "serverOverloaded",
                "cyberPolicy",
                "misalignmentPolicyViolation",
                "tooManyDenials",
                "internalServerError",
                "unauthorized",
                "badRequest",
                "threadRollbackFailed",
                "sandboxError",
                "other"
              ],
              type: "string"
            },
            {
              additionalProperties: false,
              properties: {
                httpConnectionFailed: {
                  properties: {
                    httpStatusCode: {
                      format: "uint16",
                      minimum: 0,
                      type: [
                        "integer",
                        "null"
                      ]
                    }
                  },
                  type: "object"
                }
              },
              required: [
                "httpConnectionFailed"
              ],
              title: "HttpConnectionFailedCodexErrorInfo",
              type: "object"
            },
            {
              additionalProperties: false,
              description: "Failed to connect to the response SSE stream.",
              properties: {
                responseStreamConnectionFailed: {
                  properties: {
                    httpStatusCode: {
                      format: "uint16",
                      minimum: 0,
                      type: [
                        "integer",
                        "null"
                      ]
                    }
                  },
                  type: "object"
                }
              },
              required: [
                "responseStreamConnectionFailed"
              ],
              title: "ResponseStreamConnectionFailedCodexErrorInfo",
              type: "object"
            },
            {
              additionalProperties: false,
              description: "The response SSE stream disconnected in the middle of a turn before completion.",
              properties: {
                responseStreamDisconnected: {
                  properties: {
                    httpStatusCode: {
                      format: "uint16",
                      minimum: 0,
                      type: [
                        "integer",
                        "null"
                      ]
                    }
                  },
                  type: "object"
                }
              },
              required: [
                "responseStreamDisconnected"
              ],
              title: "ResponseStreamDisconnectedCodexErrorInfo",
              type: "object"
            },
            {
              additionalProperties: false,
              description: "Reached the retry limit for responses.",
              properties: {
                responseTooManyFailedAttempts: {
                  properties: {
                    httpStatusCode: {
                      format: "uint16",
                      minimum: 0,
                      type: [
                        "integer",
                        "null"
                      ]
                    }
                  },
                  type: "object"
                }
              },
              required: [
                "responseTooManyFailedAttempts"
              ],
              title: "ResponseTooManyFailedAttemptsCodexErrorInfo",
              type: "object"
            },
            {
              additionalProperties: false,
              description: "Returned when `turn/start` or `turn/steer` is submitted while the current active turn cannot accept same-turn steering, for example `/review` or manual `/compact`.",
              properties: {
                activeTurnNotSteerable: {
                  properties: {
                    turnKind: {
                      $ref: "#/definitions/NonSteerableTurnKind"
                    }
                  },
                  required: [
                    "turnKind"
                  ],
                  type: "object"
                }
              },
              required: [
                "activeTurnNotSteerable"
              ],
              title: "ActiveTurnNotSteerableCodexErrorInfo",
              type: "object"
            },
            {
              type: [
                "string",
                "object"
              ]
            }
          ],
          description: "This translation layer make sure that we expose codex error code in camel case.\n\nWhen an upstream HTTP status is available (for example, from the Responses API or a provider), it is forwarded in `httpStatusCode` on the relevant `codexErrorInfo` variant."
        },
        CollabAgentState: {
          properties: {
            message: {
              type: [
                "string",
                "null"
              ]
            },
            status: {
              $ref: "#/definitions/CollabAgentStatus"
            }
          },
          required: [
            "status"
          ],
          type: "object"
        },
        CollabAgentStatus: {
          enum: [
            "pendingInit",
            "running",
            "interrupted",
            "completed",
            "errored",
            "shutdown",
            "notFound"
          ],
          type: "string"
        },
        CollabAgentTool: {
          enum: [
            "spawnAgent",
            "sendInput",
            "resumeAgent",
            "wait",
            "closeAgent",
            "sendMessage",
            "followupTask",
            "interruptAgent",
            "listAgents"
          ],
          type: "string"
        },
        CollabAgentToolCallStatus: {
          enum: [
            "inProgress",
            "completed",
            "failed",
            "interrupted"
          ],
          type: "string"
        },
        CollaborationMode: {
          description: "Collaboration mode for a Codex session.",
          properties: {
            mode: {
              $ref: "#/definitions/ModeKind"
            },
            settings: {
              $ref: "#/definitions/Settings"
            }
          },
          required: [
            "mode",
            "settings"
          ],
          type: "object"
        },
        CommandAction: {
          oneOf: [
            {
              properties: {
                command: {
                  type: "string"
                },
                name: {
                  type: "string"
                },
                path: {
                  $ref: "#/definitions/LegacyAppPathString"
                },
                type: {
                  enum: [
                    "read"
                  ],
                  title: "ReadCommandActionType",
                  type: "string"
                }
              },
              required: [
                "command",
                "name",
                "path",
                "type"
              ],
              title: "ReadCommandAction",
              type: "object"
            },
            {
              properties: {
                command: {
                  type: "string"
                },
                path: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "listFiles"
                  ],
                  title: "ListFilesCommandActionType",
                  type: "string"
                }
              },
              required: [
                "command",
                "type"
              ],
              title: "ListFilesCommandAction",
              type: "object"
            },
            {
              properties: {
                command: {
                  type: "string"
                },
                path: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                query: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "search"
                  ],
                  title: "SearchCommandActionType",
                  type: "string"
                }
              },
              required: [
                "command",
                "type"
              ],
              title: "SearchCommandAction",
              type: "object"
            },
            {
              properties: {
                command: {
                  type: "string"
                },
                type: {
                  enum: [
                    "unknown"
                  ],
                  title: "UnknownCommandActionType",
                  type: "string"
                }
              },
              required: [
                "command",
                "type"
              ],
              title: "UnknownCommandAction",
              type: "object"
            }
          ]
        },
        CommandExecOutputDeltaNotification: {
          description: "Base64-encoded output chunk emitted for a streaming `command/exec` request.\n\nThese notifications are connection-scoped. If the originating connection closes, the server terminates the process.",
          properties: {
            capReached: {
              description: "`true` on the final streamed chunk for a stream when `outputBytesCap` truncated later output on that stream.",
              type: "boolean"
            },
            deltaBase64: {
              description: "Base64-encoded output bytes.",
              type: "string"
            },
            processId: {
              description: "Client-supplied, connection-scoped `processId` from the original `command/exec` request.",
              type: "string"
            },
            stream: {
              allOf: [
                {
                  $ref: "#/definitions/CommandExecOutputStream"
                }
              ],
              description: "Output stream for this chunk."
            }
          },
          required: [
            "capReached",
            "deltaBase64",
            "processId",
            "stream"
          ],
          type: "object"
        },
        CommandExecOutputStream: {
          description: "Stream label for `command/exec/outputDelta` notifications.",
          oneOf: [
            {
              description: "stdout stream. PTY mode multiplexes terminal output here.",
              enum: [
                "stdout"
              ],
              type: "string"
            },
            {
              description: "stderr stream.",
              enum: [
                "stderr"
              ],
              type: "string"
            }
          ]
        },
        CommandExecutionOutputDeltaNotification: {
          properties: {
            delta: {
              type: "string"
            },
            itemId: {
              type: "string"
            },
            threadId: {
              type: "string"
            },
            turnId: {
              type: "string"
            }
          },
          required: [
            "delta",
            "itemId",
            "threadId",
            "turnId"
          ],
          type: "object"
        },
        CommandExecutionSource: {
          enum: [
            "agent",
            "userShell",
            "unifiedExecStartup",
            "unifiedExecInteraction"
          ],
          type: "string"
        },
        CommandExecutionStatus: {
          enum: [
            "inProgress",
            "completed",
            "failed",
            "declined"
          ],
          type: "string"
        },
        ConfigWarningNotification: {
          properties: {
            details: {
              description: "Optional extra guidance or error details.",
              type: [
                "string",
                "null"
              ]
            },
            path: {
              description: "Optional path to the config file that triggered the warning.",
              type: [
                "string",
                "null"
              ]
            },
            range: {
              anyOf: [
                {
                  $ref: "#/definitions/TextRange"
                },
                {
                  type: "null"
                }
              ],
              description: "Optional range for the error location inside the config file."
            },
            summary: {
              description: "Concise summary of the warning.",
              type: "string"
            }
          },
          required: [
            "summary"
          ],
          type: "object"
        },
        ContextCompactedNotification: {
          description: "Deprecated: Use `ContextCompaction` item type instead.",
          properties: {
            threadId: {
              type: "string"
            },
            turnId: {
              type: "string"
            }
          },
          required: [
            "threadId",
            "turnId"
          ],
          type: "object"
        },
        CreditsSnapshot: {
          properties: {
            balance: {
              type: [
                "string",
                "null"
              ]
            },
            hasCredits: {
              type: "boolean"
            },
            unlimited: {
              type: "boolean"
            }
          },
          required: [
            "hasCredits",
            "unlimited"
          ],
          type: "object"
        },
        DeprecationNoticeNotification: {
          properties: {
            details: {
              description: "Optional extra guidance, such as migration steps or rationale.",
              type: [
                "string",
                "null"
              ]
            },
            summary: {
              description: "Concise summary of what is deprecated.",
              type: "string"
            }
          },
          required: [
            "summary"
          ],
          type: "object"
        },
        DesktopOnboardingEntrypoint: {
          enum: [
            "life_sciences"
          ],
          type: "string"
        },
        DynamicToolCallOutputContentItem: {
          oneOf: [
            {
              properties: {
                text: {
                  type: "string"
                },
                type: {
                  enum: [
                    "inputText"
                  ],
                  title: "InputTextDynamicToolCallOutputContentItemType",
                  type: "string"
                }
              },
              required: [
                "text",
                "type"
              ],
              title: "InputTextDynamicToolCallOutputContentItem",
              type: "object"
            },
            {
              properties: {
                imageUrl: {
                  type: "string"
                },
                type: {
                  enum: [
                    "inputImage"
                  ],
                  title: "InputImageDynamicToolCallOutputContentItemType",
                  type: "string"
                }
              },
              required: [
                "imageUrl",
                "type"
              ],
              title: "InputImageDynamicToolCallOutputContentItem",
              type: "object"
            },
            {
              properties: {
                audioUrl: {
                  type: "string"
                },
                type: {
                  enum: [
                    "inputAudio"
                  ],
                  title: "InputAudioDynamicToolCallOutputContentItemType",
                  type: "string"
                }
              },
              required: [
                "audioUrl",
                "type"
              ],
              title: "InputAudioDynamicToolCallOutputContentItem",
              type: "object"
            }
          ]
        },
        DynamicToolCallStatus: {
          enum: [
            "inProgress",
            "completed",
            "failed"
          ],
          type: "string"
        },
        EnvironmentConnectionNotification: {
          properties: {
            environmentId: {
              type: "string"
            },
            threadId: {
              type: "string"
            }
          },
          required: [
            "environmentId",
            "threadId"
          ],
          type: "object"
        },
        ErrorNotification: {
          properties: {
            error: {
              $ref: "#/definitions/TurnError"
            },
            threadId: {
              type: "string"
            },
            turnId: {
              type: "string"
            },
            willRetry: {
              type: "boolean"
            }
          },
          required: [
            "error",
            "threadId",
            "turnId",
            "willRetry"
          ],
          type: "object"
        },
        ExternalAgentConfigImportCompletedNotification: {
          properties: {
            importId: {
              type: "string"
            },
            itemTypeResults: {
              items: {
                $ref: "#/definitions/ExternalAgentConfigImportTypeResult"
              },
              type: "array"
            }
          },
          required: [
            "importId",
            "itemTypeResults"
          ],
          type: "object"
        },
        ExternalAgentConfigImportItemTypeFailure: {
          properties: {
            cwd: {
              type: [
                "string",
                "null"
              ]
            },
            errorType: {
              type: [
                "string",
                "null"
              ]
            },
            failureStage: {
              type: "string"
            },
            itemType: {
              $ref: "#/definitions/ExternalAgentConfigMigrationItemType"
            },
            message: {
              type: "string"
            },
            source: {
              type: [
                "string",
                "null"
              ]
            },
            subErrorType: {
              type: [
                "string",
                "null"
              ]
            }
          },
          required: [
            "failureStage",
            "itemType",
            "message"
          ],
          type: "object"
        },
        ExternalAgentConfigImportItemTypeSuccess: {
          properties: {
            cwd: {
              type: [
                "string",
                "null"
              ]
            },
            itemType: {
              $ref: "#/definitions/ExternalAgentConfigMigrationItemType"
            },
            source: {
              type: [
                "string",
                "null"
              ]
            },
            target: {
              type: [
                "string",
                "null"
              ]
            },
            title: {
              default: null,
              description: "Original title for an imported session; null for other item types.",
              type: [
                "string",
                "null"
              ]
            }
          },
          required: [
            "itemType"
          ],
          type: "object"
        },
        ExternalAgentConfigImportProgressNotification: {
          properties: {
            importId: {
              type: "string"
            },
            itemTypeResults: {
              items: {
                $ref: "#/definitions/ExternalAgentConfigImportTypeResult"
              },
              type: "array"
            }
          },
          required: [
            "importId",
            "itemTypeResults"
          ],
          type: "object"
        },
        ExternalAgentConfigImportTypeResult: {
          properties: {
            failures: {
              items: {
                $ref: "#/definitions/ExternalAgentConfigImportItemTypeFailure"
              },
              type: "array"
            },
            itemType: {
              $ref: "#/definitions/ExternalAgentConfigMigrationItemType"
            },
            successes: {
              items: {
                $ref: "#/definitions/ExternalAgentConfigImportItemTypeSuccess"
              },
              type: "array"
            }
          },
          required: [
            "failures",
            "itemType",
            "successes"
          ],
          type: "object"
        },
        ExternalAgentConfigMigrationItemType: {
          enum: [
            "AGENTS_MD",
            "CONFIG",
            "SKILLS",
            "PLUGINS",
            "MCP_SERVER_CONFIG",
            "SUBAGENTS",
            "HOOKS",
            "COMMANDS",
            "MEMORY",
            "SESSIONS"
          ],
          type: "string"
        },
        FileChangeOutputDeltaNotification: {
          description: "Deprecated legacy notification for `apply_patch` textual output.\n\nThe server no longer emits this notification.",
          properties: {
            delta: {
              type: "string"
            },
            itemId: {
              type: "string"
            },
            threadId: {
              type: "string"
            },
            turnId: {
              type: "string"
            }
          },
          required: [
            "delta",
            "itemId",
            "threadId",
            "turnId"
          ],
          type: "object"
        },
        FileChangePatchUpdatedNotification: {
          properties: {
            changes: {
              items: {
                $ref: "#/definitions/FileUpdateChange"
              },
              type: "array"
            },
            itemId: {
              type: "string"
            },
            threadId: {
              type: "string"
            },
            turnId: {
              type: "string"
            }
          },
          required: [
            "changes",
            "itemId",
            "threadId",
            "turnId"
          ],
          type: "object"
        },
        FileSystemAccessMode: {
          enum: [
            "read",
            "write",
            "deny"
          ],
          type: "string"
        },
        FileSystemPath: {
          oneOf: [
            {
              properties: {
                path: {
                  $ref: "#/definitions/LegacyAppPathString"
                },
                type: {
                  enum: [
                    "path"
                  ],
                  title: "PathFileSystemPathType",
                  type: "string"
                }
              },
              required: [
                "path",
                "type"
              ],
              title: "PathFileSystemPath",
              type: "object"
            },
            {
              properties: {
                pattern: {
                  type: "string"
                },
                type: {
                  enum: [
                    "glob_pattern"
                  ],
                  title: "GlobPatternFileSystemPathType",
                  type: "string"
                }
              },
              required: [
                "pattern",
                "type"
              ],
              title: "GlobPatternFileSystemPath",
              type: "object"
            },
            {
              properties: {
                type: {
                  enum: [
                    "special"
                  ],
                  title: "SpecialFileSystemPathType",
                  type: "string"
                },
                value: {
                  $ref: "#/definitions/FileSystemSpecialPath"
                }
              },
              required: [
                "type",
                "value"
              ],
              title: "SpecialFileSystemPath",
              type: "object"
            }
          ]
        },
        FileSystemSandboxEntry: {
          properties: {
            access: {
              $ref: "#/definitions/FileSystemAccessMode"
            },
            path: {
              $ref: "#/definitions/FileSystemPath"
            }
          },
          required: [
            "access",
            "path"
          ],
          type: "object"
        },
        FileSystemSpecialPath: {
          oneOf: [
            {
              properties: {
                kind: {
                  enum: [
                    "root"
                  ],
                  type: "string"
                }
              },
              required: [
                "kind"
              ],
              title: "RootFileSystemSpecialPath",
              type: "object"
            },
            {
              properties: {
                kind: {
                  enum: [
                    "minimal"
                  ],
                  type: "string"
                }
              },
              required: [
                "kind"
              ],
              title: "MinimalFileSystemSpecialPath",
              type: "object"
            },
            {
              properties: {
                kind: {
                  enum: [
                    "project_roots"
                  ],
                  type: "string"
                },
                subpath: {
                  anyOf: [
                    {
                      $ref: "#/definitions/LegacyAppPathString"
                    },
                    {
                      type: "null"
                    }
                  ]
                }
              },
              required: [
                "kind"
              ],
              title: "KindFileSystemSpecialPath",
              type: "object"
            },
            {
              properties: {
                kind: {
                  enum: [
                    "tmpdir"
                  ],
                  type: "string"
                }
              },
              required: [
                "kind"
              ],
              title: "TmpdirFileSystemSpecialPath",
              type: "object"
            },
            {
              properties: {
                kind: {
                  enum: [
                    "slash_tmp"
                  ],
                  type: "string"
                }
              },
              required: [
                "kind"
              ],
              title: "SlashTmpFileSystemSpecialPath",
              type: "object"
            },
            {
              properties: {
                kind: {
                  enum: [
                    "unknown"
                  ],
                  type: "string"
                },
                path: {
                  type: "string"
                },
                subpath: {
                  anyOf: [
                    {
                      $ref: "#/definitions/LegacyAppPathString"
                    },
                    {
                      type: "null"
                    }
                  ]
                }
              },
              required: [
                "kind",
                "path"
              ],
              type: "object"
            }
          ]
        },
        FileUpdateChange: {
          properties: {
            diff: {
              type: "string"
            },
            kind: {
              $ref: "#/definitions/PatchChangeKind"
            },
            path: {
              type: "string"
            }
          },
          required: [
            "diff",
            "kind",
            "path"
          ],
          type: "object"
        },
        FsChangedNotification: {
          description: "Filesystem watch notification emitted for `fs/watch` subscribers.",
          properties: {
            changedPaths: {
              description: "File or directory paths associated with this event.",
              items: {
                $ref: "#/definitions/AbsolutePathBuf"
              },
              type: "array"
            },
            watchId: {
              description: "Watch identifier previously provided to `fs/watch`.",
              type: "string"
            }
          },
          required: [
            "changedPaths",
            "watchId"
          ],
          type: "object"
        },
        FunctionCallOutputBody: {
          anyOf: [
            {
              type: "string"
            },
            {
              items: {
                $ref: "#/definitions/FunctionCallOutputContentItem"
              },
              type: "array"
            }
          ]
        },
        FunctionCallOutputContentItem: {
          description: "Responses API compatible content items that can be returned by a tool call. This is a subset of ContentItem with the types we support as function call outputs.",
          oneOf: [
            {
              properties: {
                text: {
                  type: "string"
                },
                type: {
                  enum: [
                    "input_text"
                  ],
                  title: "InputTextFunctionCallOutputContentItemType",
                  type: "string"
                }
              },
              required: [
                "text",
                "type"
              ],
              title: "InputTextFunctionCallOutputContentItem",
              type: "object"
            },
            {
              anyOf: [
                {
                  properties: {
                    image_url: {
                      type: "string"
                    }
                  },
                  required: [
                    "image_url"
                  ],
                  title: "ImageUrlFunctionCallOutputContentItem",
                  type: "object"
                },
                {
                  properties: {
                    file_id: {
                      type: "string"
                    }
                  },
                  required: [
                    "file_id"
                  ],
                  title: "FileIdFunctionCallOutputContentItem",
                  type: "object"
                }
              ],
              properties: {
                detail: {
                  anyOf: [
                    {
                      $ref: "#/definitions/ImageDetail"
                    },
                    {
                      type: "null"
                    }
                  ]
                },
                type: {
                  enum: [
                    "input_image"
                  ],
                  title: "InputImageFunctionCallOutputContentItemType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "InputImageFunctionCallOutputContentItem",
              type: "object"
            },
            {
              properties: {
                audio_url: {
                  type: "string"
                },
                type: {
                  enum: [
                    "input_audio"
                  ],
                  title: "InputAudioFunctionCallOutputContentItemType",
                  type: "string"
                }
              },
              required: [
                "audio_url",
                "type"
              ],
              title: "InputAudioFunctionCallOutputContentItem",
              type: "object"
            },
            {
              properties: {
                encrypted_content: {
                  type: "string"
                },
                type: {
                  enum: [
                    "encrypted_content"
                  ],
                  title: "EncryptedContentFunctionCallOutputContentItemType",
                  type: "string"
                }
              },
              required: [
                "encrypted_content",
                "type"
              ],
              title: "EncryptedContentFunctionCallOutputContentItem",
              type: "object"
            }
          ]
        },
        FuzzyFileSearchMatchType: {
          enum: [
            "file",
            "directory"
          ],
          type: "string"
        },
        FuzzyFileSearchResult: {
          description: "Superset of [`codex_file_search::FileMatch`]",
          properties: {
            file_name: {
              type: "string"
            },
            indices: {
              items: {
                format: "uint32",
                minimum: 0,
                type: "integer"
              },
              type: [
                "array",
                "null"
              ]
            },
            match_type: {
              $ref: "#/definitions/FuzzyFileSearchMatchType"
            },
            path: {
              type: "string"
            },
            root: {
              type: "string"
            },
            score: {
              format: "uint32",
              minimum: 0,
              type: "integer"
            }
          },
          required: [
            "file_name",
            "match_type",
            "path",
            "root",
            "score"
          ],
          type: "object"
        },
        FuzzyFileSearchSessionCompletedNotification: {
          properties: {
            sessionId: {
              type: "string"
            }
          },
          required: [
            "sessionId"
          ],
          type: "object"
        },
        FuzzyFileSearchSessionUpdatedNotification: {
          properties: {
            files: {
              items: {
                $ref: "#/definitions/FuzzyFileSearchResult"
              },
              type: "array"
            },
            query: {
              type: "string"
            },
            sessionId: {
              type: "string"
            }
          },
          required: [
            "files",
            "query",
            "sessionId"
          ],
          type: "object"
        },
        GatewayOAuthChangedNotification: {
          properties: {
            authUrl: {
              description: "Authorization handoff, sent only to the connection that started login.",
              type: [
                "string",
                "null"
              ]
            },
            error: {
              type: [
                "string",
                "null"
              ]
            },
            providerId: {
              type: "string"
            },
            status: {
              $ref: "#/definitions/GatewayOAuthStatus"
            }
          },
          required: [
            "providerId",
            "status"
          ],
          type: "object"
        },
        GatewayOAuthStatus: {
          enum: [
            "notReady",
            "started",
            "succeeded",
            "failed"
          ],
          type: "string"
        },
        GitInfo: {
          properties: {
            branch: {
              type: [
                "string",
                "null"
              ]
            },
            originUrl: {
              type: [
                "string",
                "null"
              ]
            },
            sha: {
              type: [
                "string",
                "null"
              ]
            }
          },
          type: "object"
        },
        GuardianApprovalReview: {
          description: "[UNSTABLE] Temporary approval auto-review payload used by `item/autoApprovalReview/*` notifications. This shape is expected to change soon.",
          properties: {
            rationale: {
              type: [
                "string",
                "null"
              ]
            },
            riskLevel: {
              anyOf: [
                {
                  $ref: "#/definitions/GuardianRiskLevel"
                },
                {
                  type: "null"
                }
              ]
            },
            status: {
              $ref: "#/definitions/GuardianApprovalReviewStatus"
            },
            userAuthorization: {
              anyOf: [
                {
                  $ref: "#/definitions/GuardianUserAuthorization"
                },
                {
                  type: "null"
                }
              ]
            }
          },
          required: [
            "status"
          ],
          type: "object"
        },
        GuardianApprovalReviewAction: {
          oneOf: [
            {
              properties: {
                command: {
                  type: "string"
                },
                cwd: {
                  $ref: "#/definitions/LegacyAppPathString"
                },
                source: {
                  $ref: "#/definitions/GuardianCommandSource"
                },
                type: {
                  enum: [
                    "command"
                  ],
                  title: "CommandGuardianApprovalReviewActionType",
                  type: "string"
                }
              },
              required: [
                "command",
                "cwd",
                "source",
                "type"
              ],
              title: "CommandGuardianApprovalReviewAction",
              type: "object"
            },
            {
              properties: {
                argv: {
                  items: {
                    type: "string"
                  },
                  type: "array"
                },
                cwd: {
                  $ref: "#/definitions/AbsolutePathBuf"
                },
                program: {
                  type: "string"
                },
                source: {
                  $ref: "#/definitions/GuardianCommandSource"
                },
                type: {
                  enum: [
                    "execve"
                  ],
                  title: "ExecveGuardianApprovalReviewActionType",
                  type: "string"
                }
              },
              required: [
                "argv",
                "cwd",
                "program",
                "source",
                "type"
              ],
              title: "ExecveGuardianApprovalReviewAction",
              type: "object"
            },
            {
              description: "A child approval for input to an existing command execution item.",
              properties: {
                approvalId: {
                  type: "string"
                },
                cwd: {
                  $ref: "#/definitions/LegacyAppPathString"
                },
                processId: {
                  type: "string"
                },
                stdin: {
                  type: "string"
                },
                type: {
                  enum: [
                    "writeStdin"
                  ],
                  title: "WriteStdinGuardianApprovalReviewActionType",
                  type: "string"
                }
              },
              required: [
                "approvalId",
                "cwd",
                "processId",
                "stdin",
                "type"
              ],
              title: "WriteStdinGuardianApprovalReviewAction",
              type: "object"
            },
            {
              properties: {
                cwd: {
                  $ref: "#/definitions/LegacyAppPathString"
                },
                files: {
                  items: {
                    $ref: "#/definitions/LegacyAppPathString"
                  },
                  type: "array"
                },
                type: {
                  enum: [
                    "applyPatch"
                  ],
                  title: "ApplyPatchGuardianApprovalReviewActionType",
                  type: "string"
                }
              },
              required: [
                "cwd",
                "files",
                "type"
              ],
              title: "ApplyPatchGuardianApprovalReviewAction",
              type: "object"
            },
            {
              properties: {
                host: {
                  type: "string"
                },
                port: {
                  format: "uint16",
                  minimum: 0,
                  type: "integer"
                },
                protocol: {
                  $ref: "#/definitions/NetworkApprovalProtocol"
                },
                target: {
                  type: "string"
                },
                type: {
                  enum: [
                    "networkAccess"
                  ],
                  title: "NetworkAccessGuardianApprovalReviewActionType",
                  type: "string"
                }
              },
              required: [
                "host",
                "port",
                "protocol",
                "target",
                "type"
              ],
              title: "NetworkAccessGuardianApprovalReviewAction",
              type: "object"
            },
            {
              properties: {
                connectorId: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                connectorName: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                server: {
                  type: "string"
                },
                toolName: {
                  type: "string"
                },
                toolTitle: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "mcpToolCall"
                  ],
                  title: "McpToolCallGuardianApprovalReviewActionType",
                  type: "string"
                }
              },
              required: [
                "server",
                "toolName",
                "type"
              ],
              title: "McpToolCallGuardianApprovalReviewAction",
              type: "object"
            },
            {
              properties: {
                permissions: {
                  $ref: "#/definitions/RequestPermissionProfile"
                },
                reason: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "requestPermissions"
                  ],
                  title: "RequestPermissionsGuardianApprovalReviewActionType",
                  type: "string"
                }
              },
              required: [
                "permissions",
                "type"
              ],
              title: "RequestPermissionsGuardianApprovalReviewAction",
              type: "object"
            }
          ]
        },
        GuardianApprovalReviewStatus: {
          description: "[UNSTABLE] Lifecycle state for an approval auto-review.",
          enum: [
            "inProgress",
            "approved",
            "denied",
            "timedOut",
            "aborted"
          ],
          type: "string"
        },
        GuardianCommandSource: {
          enum: [
            "shell",
            "unifiedExec"
          ],
          type: "string"
        },
        GuardianRiskLevel: {
          description: "[UNSTABLE] Risk level assigned by approval auto-review.",
          enum: [
            "low",
            "medium",
            "high",
            "critical"
          ],
          type: "string"
        },
        GuardianUserAuthorization: {
          description: "[UNSTABLE] Authorization level assigned by approval auto-review.",
          enum: [
            "unknown",
            "low",
            "medium",
            "high"
          ],
          type: "string"
        },
        GuardianWarningNotification: {
          properties: {
            message: {
              description: "Concise guardian warning message for the user.",
              type: "string"
            },
            threadId: {
              description: "Thread target for the guardian warning.",
              type: "string"
            }
          },
          required: [
            "message",
            "threadId"
          ],
          type: "object"
        },
        HookCompletedNotification: {
          properties: {
            run: {
              $ref: "#/definitions/HookRunSummary"
            },
            threadId: {
              type: "string"
            },
            turnId: {
              type: [
                "string",
                "null"
              ]
            }
          },
          required: [
            "run",
            "threadId"
          ],
          type: "object"
        },
        HookEventName: {
          enum: [
            "preToolUse",
            "permissionRequest",
            "postToolUse",
            "preCompact",
            "postCompact",
            "sessionStart",
            "sessionEnd",
            "userPromptSubmit",
            "subagentStart",
            "subagentStop",
            "stop",
            "interrupt"
          ],
          type: "string"
        },
        HookExecutionMode: {
          enum: [
            "sync",
            "async"
          ],
          type: "string"
        },
        HookHandlerType: {
          enum: [
            "command",
            "mcpTool",
            "prompt",
            "agent"
          ],
          type: "string"
        },
        HookOutputEntry: {
          properties: {
            kind: {
              $ref: "#/definitions/HookOutputEntryKind"
            },
            text: {
              type: "string"
            }
          },
          required: [
            "kind",
            "text"
          ],
          type: "object"
        },
        HookOutputEntryKind: {
          enum: [
            "warning",
            "stop",
            "feedback",
            "context",
            "error"
          ],
          type: "string"
        },
        HookPromptFragment: {
          properties: {
            hookRunId: {
              type: "string"
            },
            text: {
              type: "string"
            }
          },
          required: [
            "hookRunId",
            "text"
          ],
          type: "object"
        },
        HookRunStatus: {
          enum: [
            "running",
            "completed",
            "failed",
            "blocked",
            "stopped"
          ],
          type: "string"
        },
        HookRunSummary: {
          properties: {
            completedAt: {
              format: "int64",
              type: [
                "integer",
                "null"
              ]
            },
            displayOrder: {
              format: "int64",
              type: "integer"
            },
            durationMs: {
              format: "int64",
              type: [
                "integer",
                "null"
              ]
            },
            entries: {
              items: {
                $ref: "#/definitions/HookOutputEntry"
              },
              type: "array"
            },
            eventName: {
              $ref: "#/definitions/HookEventName"
            },
            executionMode: {
              $ref: "#/definitions/HookExecutionMode"
            },
            handlerType: {
              $ref: "#/definitions/HookHandlerType"
            },
            id: {
              type: "string"
            },
            scope: {
              $ref: "#/definitions/HookScope"
            },
            source: {
              allOf: [
                {
                  $ref: "#/definitions/HookSource"
                }
              ],
              default: "unknown"
            },
            sourcePath: {
              $ref: "#/definitions/AbsolutePathBuf"
            },
            startedAt: {
              format: "int64",
              type: "integer"
            },
            status: {
              $ref: "#/definitions/HookRunStatus"
            },
            statusMessage: {
              type: [
                "string",
                "null"
              ]
            }
          },
          required: [
            "displayOrder",
            "entries",
            "eventName",
            "executionMode",
            "handlerType",
            "id",
            "scope",
            "sourcePath",
            "startedAt",
            "status"
          ],
          type: "object"
        },
        HookScope: {
          enum: [
            "thread",
            "turn"
          ],
          type: "string"
        },
        HookSource: {
          enum: [
            "system",
            "user",
            "project",
            "mdm",
            "sessionFlags",
            "plugin",
            "cloudRequirements",
            "cloudManagedConfig",
            "legacyManagedConfigFile",
            "legacyManagedConfigMdm",
            "unknown"
          ],
          type: "string"
        },
        HookStartedNotification: {
          properties: {
            run: {
              $ref: "#/definitions/HookRunSummary"
            },
            threadId: {
              type: "string"
            },
            turnId: {
              type: [
                "string",
                "null"
              ]
            }
          },
          required: [
            "run",
            "threadId"
          ],
          type: "object"
        },
        ImageDetail: {
          enum: [
            "auto",
            "low",
            "high",
            "original"
          ],
          type: "string"
        },
        ImageGenerationFailure: {
          oneOf: [
            {
              properties: {
                limitId: {
                  type: "string"
                },
                resetsAt: {
                  format: "int64",
                  type: [
                    "integer",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "usageLimitExceeded"
                  ],
                  title: "UsageLimitExceededImageGenerationFailureType",
                  type: "string"
                }
              },
              required: [
                "limitId",
                "type"
              ],
              title: "UsageLimitExceededImageGenerationFailure",
              type: "object"
            }
          ]
        },
        ItemCompletedNotification: {
          properties: {
            completedAtMs: {
              description: "Unix timestamp (in milliseconds) when this item lifecycle completed.",
              format: "int64",
              type: "integer"
            },
            item: {
              $ref: "#/definitions/ThreadItem"
            },
            threadId: {
              type: "string"
            },
            turnId: {
              type: "string"
            }
          },
          required: [
            "completedAtMs",
            "item",
            "threadId",
            "turnId"
          ],
          type: "object"
        },
        ItemGuardianApprovalReviewCompletedNotification: {
          description: "[UNSTABLE] Temporary notification payload for approval auto-review. This shape is expected to change soon.",
          properties: {
            action: {
              $ref: "#/definitions/GuardianApprovalReviewAction"
            },
            completedAtMs: {
              description: "Unix timestamp (in milliseconds) when this review completed.",
              format: "int64",
              type: "integer"
            },
            decisionSource: {
              $ref: "#/definitions/AutoReviewDecisionSource"
            },
            review: {
              $ref: "#/definitions/GuardianApprovalReview"
            },
            reviewId: {
              description: "Stable identifier for this review.",
              type: "string"
            },
            startedAtMs: {
              description: "Unix timestamp (in milliseconds) when this review started.",
              format: "int64",
              type: "integer"
            },
            targetItemId: {
              description: "Identifier for the reviewed item or tool call when one exists.\n\nIn most cases, one review maps to one target item. The exceptions are - execve reviews, where a single command may contain multiple execve calls to review (only possible when using the shell_zsh_fork feature) - stdin reviews, which refer to the existing parent command item and have a separate approval ID in the action payload - network policy reviews, where there is no target item\n\nA network call is triggered by a CommandExecution item, so having a target_item_id set to the CommandExecution item would be misleading because the review is about the network call, not the command execution. Therefore, target_item_id is set to None for network policy reviews.",
              type: [
                "string",
                "null"
              ]
            },
            threadId: {
              type: "string"
            },
            turnId: {
              type: "string"
            }
          },
          required: [
            "action",
            "completedAtMs",
            "decisionSource",
            "review",
            "reviewId",
            "startedAtMs",
            "threadId",
            "turnId"
          ],
          type: "object"
        },
        ItemGuardianApprovalReviewStartedNotification: {
          description: "[UNSTABLE] Temporary notification payload for approval auto-review. This shape is expected to change soon.",
          properties: {
            action: {
              $ref: "#/definitions/GuardianApprovalReviewAction"
            },
            review: {
              $ref: "#/definitions/GuardianApprovalReview"
            },
            reviewId: {
              description: "Stable identifier for this review.",
              type: "string"
            },
            startedAtMs: {
              description: "Unix timestamp (in milliseconds) when this review started.",
              format: "int64",
              type: "integer"
            },
            targetItemId: {
              description: "Identifier for the reviewed item or tool call when one exists.\n\nIn most cases, one review maps to one target item. The exceptions are - execve reviews, where a single command may contain multiple execve calls to review (only possible when using the shell_zsh_fork feature) - stdin reviews, which refer to the existing parent command item and have a separate approval ID in the action payload - network policy reviews, where there is no target item\n\nA network call is triggered by a CommandExecution item, so having a target_item_id set to the CommandExecution item would be misleading because the review is about the network call, not the command execution. Therefore, target_item_id is set to None for network policy reviews.",
              type: [
                "string",
                "null"
              ]
            },
            threadId: {
              type: "string"
            },
            turnId: {
              type: "string"
            }
          },
          required: [
            "action",
            "review",
            "reviewId",
            "startedAtMs",
            "threadId",
            "turnId"
          ],
          type: "object"
        },
        ItemStartedNotification: {
          properties: {
            item: {
              $ref: "#/definitions/ThreadItem"
            },
            startedAtMs: {
              description: "Unix timestamp (in milliseconds) when this item lifecycle started.",
              format: "int64",
              type: "integer"
            },
            threadId: {
              type: "string"
            },
            turnId: {
              type: "string"
            }
          },
          required: [
            "item",
            "startedAtMs",
            "threadId",
            "turnId"
          ],
          type: "object"
        },
        LegacyAppPathString: {
          type: "string"
        },
        McpAppDisplayMode: {
          enum: [
            "inline",
            "fullscreen"
          ],
          type: "string"
        },
        McpAppUi: {
          description: "UI resource and display preference for model invocations, captured from the tool descriptor.",
          properties: {
            preferredModelDisplayMode: {
              $ref: "#/definitions/McpAppDisplayMode"
            },
            resourceUri: {
              type: "string"
            }
          },
          required: [
            "preferredModelDisplayMode",
            "resourceUri"
          ],
          type: "object"
        },
        McpServerEventNotification: {
          properties: {
            method: {
              type: "string"
            },
            params: true
          },
          required: [
            "method",
            "params"
          ],
          type: "object"
        },
        McpServerEventStreamNotification: {
          properties: {
            notification: {
              $ref: "#/definitions/McpServerEventNotification"
            },
            subscriptionId: {
              type: "string"
            }
          },
          required: [
            "notification",
            "subscriptionId"
          ],
          type: "object"
        },
        McpServerOauthLoginCompletedNotification: {
          properties: {
            error: {
              type: [
                "string",
                "null"
              ]
            },
            loginId: {
              description: "Identifies the explicit login attempt. Older servers omit this field.",
              type: [
                "string",
                "null"
              ]
            },
            name: {
              type: "string"
            },
            success: {
              type: "boolean"
            },
            threadId: {
              type: [
                "string",
                "null"
              ]
            }
          },
          required: [
            "name",
            "success"
          ],
          type: "object"
        },
        McpServerStartupFailureReason: {
          enum: [
            "reauthenticationRequired"
          ],
          type: "string"
        },
        McpServerStartupState: {
          enum: [
            "starting",
            "ready",
            "failed",
            "cancelled"
          ],
          type: "string"
        },
        McpServerStatusUpdatedNotification: {
          properties: {
            error: {
              type: [
                "string",
                "null"
              ]
            },
            failureReason: {
              anyOf: [
                {
                  $ref: "#/definitions/McpServerStartupFailureReason"
                },
                {
                  type: "null"
                }
              ]
            },
            name: {
              type: "string"
            },
            status: {
              $ref: "#/definitions/McpServerStartupState"
            },
            threadId: {
              type: [
                "string",
                "null"
              ]
            }
          },
          required: [
            "name",
            "status"
          ],
          type: "object"
        },
        McpToolCallAppContext: {
          properties: {
            actionName: {
              type: [
                "string",
                "null"
              ]
            },
            appName: {
              type: [
                "string",
                "null"
              ]
            },
            connectorId: {
              type: "string"
            },
            linkId: {
              type: [
                "string",
                "null"
              ]
            },
            resourceUri: {
              type: [
                "string",
                "null"
              ]
            }
          },
          required: [
            "connectorId"
          ],
          type: "object"
        },
        McpToolCallError: {
          properties: {
            message: {
              type: "string"
            }
          },
          required: [
            "message"
          ],
          type: "object"
        },
        McpToolCallProgressNotification: {
          properties: {
            itemId: {
              type: "string"
            },
            message: {
              type: "string"
            },
            threadId: {
              type: "string"
            },
            turnId: {
              type: "string"
            }
          },
          required: [
            "itemId",
            "message",
            "threadId",
            "turnId"
          ],
          type: "object"
        },
        McpToolCallResult: {
          properties: {
            _meta: true,
            content: {
              items: true,
              type: "array"
            },
            structuredContent: true
          },
          required: [
            "content"
          ],
          type: "object"
        },
        McpToolCallStatus: {
          enum: [
            "inProgress",
            "completed",
            "failed"
          ],
          type: "string"
        },
        MemoryCitation: {
          properties: {
            entries: {
              items: {
                $ref: "#/definitions/MemoryCitationEntry"
              },
              type: "array"
            },
            threadIds: {
              items: {
                type: "string"
              },
              type: "array"
            }
          },
          required: [
            "entries",
            "threadIds"
          ],
          type: "object"
        },
        MemoryCitationEntry: {
          properties: {
            lineEnd: {
              format: "uint32",
              minimum: 0,
              type: "integer"
            },
            lineStart: {
              format: "uint32",
              minimum: 0,
              type: "integer"
            },
            note: {
              type: "string"
            },
            path: {
              type: "string"
            }
          },
          required: [
            "lineEnd",
            "lineStart",
            "note",
            "path"
          ],
          type: "object"
        },
        MessagePhase: {
          description: 'Classifies assistant text as commentary, a partial answer, or a terminal answer.\n\nProviders do not emit this consistently, so callers must treat `None` as "phase unknown" and keep compatibility behavior for legacy models.',
          oneOf: [
            {
              description: "Mid-turn assistant text (for example preamble/progress narration).\n\nAdditional tool calls or assistant output may follow before turn completion.",
              enum: [
                "commentary"
              ],
              type: "string"
            },
            {
              description: "Stable answer text that may be followed by more assistant output or tools.",
              enum: [
                "partial_answer"
              ],
              type: "string"
            },
            {
              description: "The assistant's declared terminal answer text for the current turn.",
              enum: [
                "final_answer"
              ],
              type: "string"
            }
          ]
        },
        MisalignmentErrorDetails: {
          properties: {
            detailedExplanation: {
              description: "A substantive localized explanation is required before offering continuation.",
              type: [
                "string",
                "null"
              ]
            },
            errorType: {
              description: "Open-ended classification; clients must accept categories added by Responses.",
              type: [
                "string",
                "null"
              ]
            },
            reviewTarget: {
              description: "Opaque server-issued block target. Presence alone does not enable target-based continuation.",
              type: [
                "string",
                "null"
              ]
            },
            steer: {
              anyOf: [
                {
                  $ref: "#/definitions/MisalignmentSteer"
                },
                {
                  type: "null"
                }
              ],
              description: "Instruction to submit as the next turn's user input if continuation is confirmed."
            }
          },
          type: "object"
        },
        MisalignmentSteer: {
          properties: {
            message: {
              type: "string"
            }
          },
          required: [
            "message"
          ],
          type: "object"
        },
        ModeKind: {
          description: "Initial collaboration mode to use when the TUI starts.",
          enum: [
            "plan",
            "default"
          ],
          type: "string"
        },
        ModelRerouteReason: {
          enum: [
            "highRiskCyberActivity"
          ],
          type: "string"
        },
        ModelReroutedNotification: {
          properties: {
            fromModel: {
              type: "string"
            },
            reason: {
              $ref: "#/definitions/ModelRerouteReason"
            },
            threadId: {
              type: "string"
            },
            toModel: {
              type: "string"
            },
            turnId: {
              type: "string"
            }
          },
          required: [
            "fromModel",
            "reason",
            "threadId",
            "toModel",
            "turnId"
          ],
          type: "object"
        },
        ModelSafetyBufferingUpdatedNotification: {
          properties: {
            fasterModel: {
              type: [
                "string",
                "null"
              ]
            },
            model: {
              type: "string"
            },
            reasons: {
              items: {
                type: "string"
              },
              type: "array"
            },
            showBufferingUi: {
              type: "boolean"
            },
            threadId: {
              type: "string"
            },
            turnId: {
              type: "string"
            },
            useCases: {
              items: {
                type: "string"
              },
              type: "array"
            }
          },
          required: [
            "model",
            "reasons",
            "showBufferingUi",
            "threadId",
            "turnId",
            "useCases"
          ],
          type: "object"
        },
        ModelVerification: {
          enum: [
            "trustedAccessForCyber"
          ],
          type: "string"
        },
        ModelVerificationNotification: {
          properties: {
            threadId: {
              type: "string"
            },
            turnId: {
              type: "string"
            },
            verifications: {
              items: {
                $ref: "#/definitions/ModelVerification"
              },
              type: "array"
            }
          },
          required: [
            "threadId",
            "turnId",
            "verifications"
          ],
          type: "object"
        },
        MultiAgentMode: {
          description: "Controls the effective multi-agent delegation instructions for a turn. `custom` means the configured mode hint defines the policy instead of a built-in policy.",
          oneOf: [
            {
              enum: [
                "explicitRequestOnly",
                "proactive"
              ],
              type: "string"
            },
            {
              additionalProperties: false,
              properties: {
                custom: {
                  type: "string"
                }
              },
              required: [
                "custom"
              ],
              title: "CustomMultiAgentMode",
              type: "object"
            }
          ]
        },
        NetworkAccess: {
          enum: [
            "restricted",
            "enabled"
          ],
          type: "string"
        },
        NetworkApprovalProtocol: {
          enum: [
            "http",
            "https",
            "socks5Tcp",
            "socks5Udp"
          ],
          type: "string"
        },
        NonSteerableTurnKind: {
          enum: [
            "review",
            "compact"
          ],
          type: "string"
        },
        PatchApplyStatus: {
          enum: [
            "inProgress",
            "completed",
            "failed",
            "declined"
          ],
          type: "string"
        },
        PatchChangeKind: {
          oneOf: [
            {
              properties: {
                type: {
                  enum: [
                    "add"
                  ],
                  title: "AddPatchChangeKindType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "AddPatchChangeKind",
              type: "object"
            },
            {
              properties: {
                type: {
                  enum: [
                    "delete"
                  ],
                  title: "DeletePatchChangeKindType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "DeletePatchChangeKind",
              type: "object"
            },
            {
              properties: {
                move_path: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "update"
                  ],
                  title: "UpdatePatchChangeKindType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "UpdatePatchChangeKind",
              type: "object"
            }
          ]
        },
        Personality: {
          description: "Deprecated: `friendly` and `pragmatic` no longer select a style.",
          enum: [
            "none",
            "friendly",
            "pragmatic"
          ],
          type: "string"
        },
        PlanDeltaNotification: {
          description: "EXPERIMENTAL - proposed plan streaming deltas for plan items. Clients should not assume concatenated deltas match the completed plan item content.",
          properties: {
            delta: {
              type: "string"
            },
            itemId: {
              type: "string"
            },
            threadId: {
              type: "string"
            },
            turnId: {
              type: "string"
            }
          },
          required: [
            "delta",
            "itemId",
            "threadId",
            "turnId"
          ],
          type: "object"
        },
        PlanType: {
          enum: [
            "free",
            "go",
            "plus",
            "pro",
            "prolite",
            "promax",
            "team",
            "self_serve_business_prolite",
            "self_serve_business_usage_based",
            "business",
            "ent26",
            "enterprise_cbp_automation",
            "enterprise_cbp_usage_based",
            "enterprise",
            "edu",
            "edu_plus",
            "edu_pro",
            "unknown"
          ],
          type: "string"
        },
        ProcessExitedNotification: {
          description: "Final process exit notification for `process/spawn`.",
          properties: {
            exitCode: {
              description: "Process exit code.",
              format: "int32",
              type: "integer"
            },
            processHandle: {
              description: "Client-supplied, connection-scoped `processHandle` from `process/spawn`.",
              type: "string"
            },
            stderr: {
              description: "Buffered stderr capture.\n\nEmpty when stderr was streamed via `process/outputDelta`.",
              type: "string"
            },
            stderrCapReached: {
              description: "Whether stderr reached `outputBytesCap`.\n\nIn streaming mode, stderr is empty and cap state is also reported on the final stderr `process/outputDelta` notification.",
              type: "boolean"
            },
            stdout: {
              description: "Buffered stdout capture.\n\nEmpty when stdout was streamed via `process/outputDelta`.",
              type: "string"
            },
            stdoutCapReached: {
              description: "Whether stdout reached `outputBytesCap`.\n\nIn streaming mode, stdout is empty and cap state is also reported on the final stdout `process/outputDelta` notification.",
              type: "boolean"
            }
          },
          required: [
            "exitCode",
            "processHandle",
            "stderr",
            "stderrCapReached",
            "stdout",
            "stdoutCapReached"
          ],
          type: "object"
        },
        ProcessOutputDeltaNotification: {
          description: "Base64-encoded output chunk emitted for a streaming `process/spawn` request.",
          properties: {
            capReached: {
              description: "True on the final streamed chunk for this stream when output was truncated by `outputBytesCap`.",
              type: "boolean"
            },
            deltaBase64: {
              description: "Base64-encoded output bytes.",
              type: "string"
            },
            processHandle: {
              description: "Client-supplied, connection-scoped `processHandle` from `process/spawn`.",
              type: "string"
            },
            stream: {
              allOf: [
                {
                  $ref: "#/definitions/ProcessOutputStream"
                }
              ],
              description: "Output stream this chunk belongs to."
            }
          },
          required: [
            "capReached",
            "deltaBase64",
            "processHandle",
            "stream"
          ],
          type: "object"
        },
        ProcessOutputStream: {
          description: "Stream label for `process/outputDelta` notifications.",
          oneOf: [
            {
              description: "stdout stream. PTY mode multiplexes terminal output here.",
              enum: [
                "stdout"
              ],
              type: "string"
            },
            {
              description: "stderr stream.",
              enum: [
                "stderr"
              ],
              type: "string"
            }
          ]
        },
        ProjectChangeType: {
          enum: [
            "created",
            "updated",
            "deleted"
          ],
          type: "string"
        },
        ProjectChangedNotification: {
          properties: {
            changeType: {
              $ref: "#/definitions/ProjectChangeType"
            },
            projectId: {
              type: "string"
            }
          },
          required: [
            "changeType",
            "projectId"
          ],
          type: "object"
        },
        RateLimitReachedType: {
          enum: [
            "rate_limit_reached",
            "workspace_owner_credits_depleted",
            "workspace_member_credits_depleted",
            "workspace_owner_usage_limit_reached",
            "workspace_member_usage_limit_reached"
          ],
          type: "string"
        },
        RateLimitSnapshot: {
          properties: {
            credits: {
              anyOf: [
                {
                  $ref: "#/definitions/CreditsSnapshot"
                },
                {
                  type: "null"
                }
              ]
            },
            individualLimit: {
              anyOf: [
                {
                  $ref: "#/definitions/SpendControlLimitSnapshot"
                },
                {
                  type: "null"
                }
              ]
            },
            limitId: {
              type: [
                "string",
                "null"
              ]
            },
            limitName: {
              type: [
                "string",
                "null"
              ]
            },
            normalModelSlug: {
              description: "Normal model whose display name and reasoning options describe this quota alias.",
              type: [
                "string",
                "null"
              ]
            },
            planType: {
              anyOf: [
                {
                  $ref: "#/definitions/PlanType"
                },
                {
                  type: "null"
                }
              ]
            },
            primary: {
              anyOf: [
                {
                  $ref: "#/definitions/RateLimitWindow"
                },
                {
                  type: "null"
                }
              ]
            },
            rateLimitReachedType: {
              anyOf: [
                {
                  $ref: "#/definitions/RateLimitReachedType"
                },
                {
                  type: "null"
                }
              ]
            },
            secondary: {
              anyOf: [
                {
                  $ref: "#/definitions/RateLimitWindow"
                },
                {
                  type: "null"
                }
              ]
            },
            spendControlReached: {
              description: "Backend-reported spend-control state. `None` is unavailable, not a sparse-update recovery.",
              type: [
                "boolean",
                "null"
              ]
            }
          },
          type: "object"
        },
        RateLimitWindow: {
          properties: {
            resetsAt: {
              format: "int64",
              type: [
                "integer",
                "null"
              ]
            },
            usedPercent: {
              format: "int32",
              type: "integer"
            },
            windowDurationMins: {
              format: "int64",
              type: [
                "integer",
                "null"
              ]
            }
          },
          required: [
            "usedPercent"
          ],
          type: "object"
        },
        RealtimeConversationVersion: {
          enum: [
            "v1",
            "v2",
            "v3"
          ],
          type: "string"
        },
        ReasoningEffort: {
          description: "A non-empty reasoning effort value advertised by the model.",
          minLength: 1,
          type: "string"
        },
        ReasoningSummary: {
          description: "A summary of the reasoning performed by the model. This can be useful for debugging and understanding the model's reasoning process. See https://platform.openai.com/docs/guides/reasoning?api-mode=responses#reasoning-summaries",
          oneOf: [
            {
              enum: [
                "auto",
                "concise",
                "detailed"
              ],
              type: "string"
            },
            {
              description: "Option to disable reasoning summaries.",
              enum: [
                "none"
              ],
              type: "string"
            }
          ]
        },
        ReasoningSummaryPartAddedNotification: {
          properties: {
            itemId: {
              type: "string"
            },
            summaryIndex: {
              format: "int64",
              type: "integer"
            },
            threadId: {
              type: "string"
            },
            turnId: {
              type: "string"
            }
          },
          required: [
            "itemId",
            "summaryIndex",
            "threadId",
            "turnId"
          ],
          type: "object"
        },
        ReasoningSummaryTextDeltaNotification: {
          properties: {
            delta: {
              type: "string"
            },
            itemId: {
              type: "string"
            },
            summaryIndex: {
              format: "int64",
              type: "integer"
            },
            threadId: {
              type: "string"
            },
            turnId: {
              type: "string"
            }
          },
          required: [
            "delta",
            "itemId",
            "summaryIndex",
            "threadId",
            "turnId"
          ],
          type: "object"
        },
        ReasoningTextDeltaNotification: {
          properties: {
            contentIndex: {
              format: "int64",
              type: "integer"
            },
            delta: {
              type: "string"
            },
            itemId: {
              type: "string"
            },
            threadId: {
              type: "string"
            },
            turnId: {
              type: "string"
            }
          },
          required: [
            "contentIndex",
            "delta",
            "itemId",
            "threadId",
            "turnId"
          ],
          type: "object"
        },
        RemoteControlConnectionStatus: {
          enum: [
            "disabled",
            "connecting",
            "connected",
            "errored"
          ],
          type: "string"
        },
        RemoteControlStatusChangedNotification: {
          description: "Current remote-control connection status and remote identity exposed to clients.",
          properties: {
            environmentId: {
              type: [
                "string",
                "null"
              ]
            },
            installationId: {
              type: "string"
            },
            serverName: {
              type: "string"
            },
            status: {
              $ref: "#/definitions/RemoteControlConnectionStatus"
            }
          },
          required: [
            "installationId",
            "serverName",
            "status"
          ],
          type: "object"
        },
        RequestId: {
          anyOf: [
            {
              type: "string"
            },
            {
              format: "int64",
              type: "integer"
            }
          ]
        },
        RequestPermissionProfile: {
          additionalProperties: false,
          properties: {
            fileSystem: {
              anyOf: [
                {
                  $ref: "#/definitions/AdditionalFileSystemPermissions"
                },
                {
                  type: "null"
                }
              ]
            },
            network: {
              anyOf: [
                {
                  $ref: "#/definitions/AdditionalNetworkPermissions"
                },
                {
                  type: "null"
                }
              ]
            }
          },
          type: "object"
        },
        SandboxPolicy: {
          oneOf: [
            {
              properties: {
                type: {
                  enum: [
                    "dangerFullAccess"
                  ],
                  title: "DangerFullAccessSandboxPolicyType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "DangerFullAccessSandboxPolicy",
              type: "object"
            },
            {
              properties: {
                networkAccess: {
                  default: false,
                  type: "boolean"
                },
                type: {
                  enum: [
                    "readOnly"
                  ],
                  title: "ReadOnlySandboxPolicyType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "ReadOnlySandboxPolicy",
              type: "object"
            },
            {
              properties: {
                networkAccess: {
                  allOf: [
                    {
                      $ref: "#/definitions/NetworkAccess"
                    }
                  ],
                  default: "restricted"
                },
                type: {
                  enum: [
                    "externalSandbox"
                  ],
                  title: "ExternalSandboxSandboxPolicyType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "ExternalSandboxSandboxPolicy",
              type: "object"
            },
            {
              properties: {
                excludeSlashTmp: {
                  default: false,
                  type: "boolean"
                },
                excludeTmpdirEnvVar: {
                  default: false,
                  type: "boolean"
                },
                networkAccess: {
                  default: false,
                  type: "boolean"
                },
                type: {
                  enum: [
                    "workspaceWrite"
                  ],
                  title: "WorkspaceWriteSandboxPolicyType",
                  type: "string"
                },
                writableRoots: {
                  default: [],
                  items: {
                    $ref: "#/definitions/AbsolutePathBuf"
                  },
                  type: "array"
                }
              },
              required: [
                "type"
              ],
              title: "WorkspaceWriteSandboxPolicy",
              type: "object"
            }
          ]
        },
        ServerRequestResolvedNotification: {
          properties: {
            requestId: {
              $ref: "#/definitions/RequestId"
            },
            threadId: {
              type: "string"
            }
          },
          required: [
            "requestId",
            "threadId"
          ],
          type: "object"
        },
        SessionSource: {
          oneOf: [
            {
              enum: [
                "cli",
                "vscode",
                "exec",
                "appServer",
                "unknown"
              ],
              type: "string"
            },
            {
              additionalProperties: false,
              properties: {
                custom: {
                  type: "string"
                }
              },
              required: [
                "custom"
              ],
              title: "CustomSessionSource",
              type: "object"
            },
            {
              additionalProperties: false,
              properties: {
                subAgent: {
                  $ref: "#/definitions/SubAgentSource"
                }
              },
              required: [
                "subAgent"
              ],
              title: "SubAgentSessionSource",
              type: "object"
            }
          ]
        },
        Settings: {
          description: "Settings for a collaboration mode.",
          properties: {
            developer_instructions: {
              type: [
                "string",
                "null"
              ]
            },
            model: {
              type: "string"
            },
            reasoning_effort: {
              anyOf: [
                {
                  $ref: "#/definitions/ReasoningEffort"
                },
                {
                  type: "null"
                }
              ]
            }
          },
          required: [
            "model"
          ],
          type: "object"
        },
        SkillsChangedNotification: {
          description: "Notification emitted when watched local skill files change.\n\nTreat this as an invalidation signal and re-run `skills/list` with the client's current parameters when refreshed skill metadata is needed.",
          type: "object"
        },
        SpendControlLimitSnapshot: {
          properties: {
            limit: {
              type: "string"
            },
            remainingPercent: {
              format: "int32",
              type: "integer"
            },
            resetsAt: {
              format: "int64",
              type: "integer"
            },
            used: {
              type: "string"
            }
          },
          required: [
            "limit",
            "remainingPercent",
            "resetsAt",
            "used"
          ],
          type: "object"
        },
        StrictReviewRequiredNotification: {
          properties: {
            startedAtMs: {
              description: "Unix timestamp (in milliseconds) when this review started.",
              format: "int64",
              type: "integer"
            },
            threadId: {
              type: "string"
            },
            turnId: {
              type: "string"
            }
          },
          required: [
            "startedAtMs",
            "threadId",
            "turnId"
          ],
          type: "object"
        },
        SubAgentActivityKind: {
          enum: [
            "started",
            "interacted",
            "interrupted",
            "completed"
          ],
          type: "string"
        },
        SubAgentSource: {
          oneOf: [
            {
              enum: [
                "review",
                "compact",
                "memory_consolidation"
              ],
              type: "string"
            },
            {
              additionalProperties: false,
              properties: {
                thread_spawn: {
                  properties: {
                    agent_nickname: {
                      default: null,
                      type: [
                        "string",
                        "null"
                      ]
                    },
                    agent_path: {
                      anyOf: [
                        {
                          $ref: "#/definitions/AgentPath"
                        },
                        {
                          type: "null"
                        }
                      ],
                      default: null
                    },
                    agent_role: {
                      default: null,
                      type: [
                        "string",
                        "null"
                      ]
                    },
                    depth: {
                      format: "int32",
                      type: "integer"
                    },
                    parent_thread_id: {
                      $ref: "#/definitions/ThreadId"
                    }
                  },
                  required: [
                    "depth",
                    "parent_thread_id"
                  ],
                  type: "object"
                }
              },
              required: [
                "thread_spawn"
              ],
              title: "ThreadSpawnSubAgentSource",
              type: "object"
            },
            {
              additionalProperties: false,
              properties: {
                other: {
                  type: "string"
                }
              },
              required: [
                "other"
              ],
              title: "OtherSubAgentSource",
              type: "object"
            }
          ]
        },
        TerminalInteractionNotification: {
          properties: {
            itemId: {
              type: "string"
            },
            processId: {
              type: "string"
            },
            stdin: {
              type: "string"
            },
            threadId: {
              type: "string"
            },
            turnId: {
              type: "string"
            }
          },
          required: [
            "itemId",
            "processId",
            "stdin",
            "threadId",
            "turnId"
          ],
          type: "object"
        },
        TextElement: {
          properties: {
            byteRange: {
              allOf: [
                {
                  $ref: "#/definitions/ByteRange"
                }
              ],
              description: "Byte range in the parent `text` buffer that this element occupies."
            },
            placeholder: {
              description: "Optional human-readable placeholder for the element, displayed in the UI.",
              type: [
                "string",
                "null"
              ]
            }
          },
          required: [
            "byteRange"
          ],
          type: "object"
        },
        TextPosition: {
          properties: {
            column: {
              description: "1-based column number (in Unicode scalar values).",
              format: "uint",
              minimum: 0,
              type: "integer"
            },
            line: {
              description: "1-based line number.",
              format: "uint",
              minimum: 0,
              type: "integer"
            }
          },
          required: [
            "column",
            "line"
          ],
          type: "object"
        },
        TextRange: {
          properties: {
            end: {
              $ref: "#/definitions/TextPosition"
            },
            start: {
              $ref: "#/definitions/TextPosition"
            }
          },
          required: [
            "end",
            "start"
          ],
          type: "object"
        },
        Thread: {
          properties: {
            agentNickname: {
              description: "Optional random unique nickname assigned to an AgentControl-spawned sub-agent.",
              type: [
                "string",
                "null"
              ]
            },
            agentRole: {
              description: "Optional role (agent_role) assigned to an AgentControl-spawned sub-agent.",
              type: [
                "string",
                "null"
              ]
            },
            cliVersion: {
              description: "Version of the CLI that created the thread.",
              type: "string"
            },
            createdAt: {
              description: "Unix timestamp (in seconds) when the thread was created.",
              format: "int64",
              type: "integer"
            },
            cwd: {
              allOf: [
                {
                  $ref: "#/definitions/AbsolutePathBuf"
                }
              ],
              description: "Working directory captured for the thread."
            },
            ephemeral: {
              description: "Whether the thread is ephemeral and should not be materialized on disk.",
              type: "boolean"
            },
            forkedFromId: {
              description: "Source thread id when this thread was created by forking another thread.",
              type: [
                "string",
                "null"
              ]
            },
            gitInfo: {
              anyOf: [
                {
                  $ref: "#/definitions/GitInfo"
                },
                {
                  type: "null"
                }
              ],
              description: "Optional Git metadata captured when the thread was created."
            },
            historyMode: {
              allOf: [
                {
                  $ref: "#/definitions/ThreadHistoryMode"
                }
              ],
              default: "legacy",
              description: "Persisted thread history contract selected when this thread was created."
            },
            id: {
              description: "Identifier for this thread. Codex-generated thread IDs are UUIDv7.",
              type: "string"
            },
            model: {
              description: "Current configured model when loaded, otherwise the latest persisted model. Null when unavailable. This is not per-turn execution telemetry.",
              type: [
                "string",
                "null"
              ]
            },
            modelProvider: {
              description: "Model provider used for this thread (for example, 'openai').",
              type: "string"
            },
            name: {
              description: "Optional user-facing thread title.",
              type: [
                "string",
                "null"
              ]
            },
            originator: {
              description: "Originator recorded when the thread was created, independent of its current client or executor. Null when the recorded originator is unavailable.",
              type: [
                "string",
                "null"
              ]
            },
            parentThreadId: {
              description: "The ID of the parent thread. This will only be set if this thread is a subagent.",
              type: [
                "string",
                "null"
              ]
            },
            path: {
              description: "[UNSTABLE] Path to the thread on disk.",
              type: [
                "string",
                "null"
              ]
            },
            preview: {
              description: "Usually the first user message in the thread, if available.",
              type: "string"
            },
            projectId: {
              description: "Canonical project assignment owned by app-server, if any.",
              type: [
                "string",
                "null"
              ]
            },
            reasoningEffort: {
              anyOf: [
                {
                  $ref: "#/definitions/ReasoningEffort"
                },
                {
                  type: "null"
                }
              ],
              description: "Current configured reasoning effort when loaded, otherwise the latest persisted effort. Null when unset or unavailable. This is not per-turn execution telemetry."
            },
            recencyAt: {
              description: "Unix timestamp (in seconds) used for thread recency ordering.",
              format: "int64",
              type: [
                "integer",
                "null"
              ]
            },
            section: {
              anyOf: [
                {
                  $ref: "#/definitions/ThreadSection"
                },
                {
                  type: "null"
                }
              ],
              default: null,
              description: "The independently persisted section selected for this thread, if any."
            },
            sectionEnteredAt: {
              default: null,
              description: "Unix timestamp in seconds when the thread entered its current section.",
              format: "int64",
              type: [
                "integer",
                "null"
              ]
            },
            sessionId: {
              description: "Session id shared by threads that belong to the same session tree.",
              type: "string"
            },
            source: {
              allOf: [
                {
                  $ref: "#/definitions/SessionSource"
                }
              ],
              description: "Origin of the thread (CLI, VSCode, codex exec, codex app-server, etc.)."
            },
            status: {
              allOf: [
                {
                  $ref: "#/definitions/ThreadStatus"
                }
              ],
              description: "Current runtime status for the thread."
            },
            threadSource: {
              anyOf: [
                {
                  $ref: "#/definitions/ThreadSource"
                },
                {
                  type: "null"
                }
              ],
              description: "Optional analytics source classification for this thread."
            },
            turns: {
              description: "Only populated on `thread/resume`, `thread/fork`, and `thread/read` (when `includeTurns` is true) responses. For all other responses and notifications returning a Thread, the turns field will be an empty list.",
              items: {
                $ref: "#/definitions/Turn"
              },
              type: "array"
            },
            updatedAt: {
              description: "Unix timestamp (in seconds) when the thread was last updated.",
              format: "int64",
              type: "integer"
            }
          },
          required: [
            "cliVersion",
            "createdAt",
            "cwd",
            "ephemeral",
            "id",
            "modelProvider",
            "preview",
            "projectId",
            "sessionId",
            "source",
            "status",
            "turns",
            "updatedAt"
          ],
          type: "object"
        },
        ThreadActiveFlag: {
          enum: [
            "waitingOnApproval",
            "waitingOnUserInput"
          ],
          type: "string"
        },
        ThreadArchivedNotification: {
          properties: {
            threadId: {
              type: "string"
            }
          },
          required: [
            "threadId"
          ],
          type: "object"
        },
        ThreadAttachmentOperation: {
          description: "The persisted attachment change represented by a notification.",
          enum: [
            "created",
            "deleted"
          ],
          type: "string"
        },
        ThreadAttachmentUpdatedNotification: {
          description: "Notification published after a thread attachment is created or deleted.",
          properties: {
            attachmentId: {
              type: "string"
            },
            attachmentType: {
              type: "string"
            },
            identityKey: {
              type: "string"
            },
            operation: {
              $ref: "#/definitions/ThreadAttachmentOperation"
            },
            threadId: {
              type: "string"
            }
          },
          required: [
            "attachmentId",
            "attachmentType",
            "identityKey",
            "operation",
            "threadId"
          ],
          type: "object"
        },
        ThreadClosedNotification: {
          properties: {
            threadId: {
              type: "string"
            }
          },
          required: [
            "threadId"
          ],
          type: "object"
        },
        ThreadDeletedNotification: {
          properties: {
            threadId: {
              type: "string"
            }
          },
          required: [
            "threadId"
          ],
          type: "object"
        },
        ThreadEnvironment: {
          description: "An environment selected by a loaded thread, independent of connection status.",
          properties: {
            cwd: {
              $ref: "#/definitions/LegacyAppPathString"
            },
            environmentId: {
              type: "string"
            },
            runtimeWorkspaceRoots: {
              items: {
                $ref: "#/definitions/LegacyAppPathString"
              },
              type: "array"
            }
          },
          required: [
            "cwd",
            "environmentId",
            "runtimeWorkspaceRoots"
          ],
          type: "object"
        },
        ThreadExtra: {
          description: "Extra app-server data for a thread.",
          type: "object"
        },
        ThreadGoal: {
          properties: {
            createdAt: {
              format: "int64",
              type: "integer"
            },
            objective: {
              type: "string"
            },
            status: {
              $ref: "#/definitions/ThreadGoalStatus"
            },
            threadId: {
              type: "string"
            },
            timeUsedSeconds: {
              format: "int64",
              type: "integer"
            },
            tokenBudget: {
              format: "int64",
              type: [
                "integer",
                "null"
              ]
            },
            tokensUsed: {
              format: "int64",
              type: "integer"
            },
            updatedAt: {
              format: "int64",
              type: "integer"
            }
          },
          required: [
            "createdAt",
            "objective",
            "status",
            "threadId",
            "timeUsedSeconds",
            "tokensUsed",
            "updatedAt"
          ],
          type: "object"
        },
        ThreadGoalClearedNotification: {
          properties: {
            threadId: {
              type: "string"
            }
          },
          required: [
            "threadId"
          ],
          type: "object"
        },
        ThreadGoalStatus: {
          enum: [
            "active",
            "paused",
            "blocked",
            "usageLimited",
            "budgetLimited",
            "complete"
          ],
          type: "string"
        },
        ThreadGoalUpdatedNotification: {
          properties: {
            goal: {
              $ref: "#/definitions/ThreadGoal"
            },
            threadId: {
              type: "string"
            },
            turnId: {
              type: [
                "string",
                "null"
              ]
            }
          },
          required: [
            "goal",
            "threadId"
          ],
          type: "object"
        },
        ThreadHistoryMode: {
          enum: [
            "legacy",
            "paginated"
          ],
          type: "string"
        },
        ThreadId: {
          type: "string"
        },
        ThreadItem: {
          oneOf: [
            {
              properties: {
                clientId: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                content: {
                  items: {
                    $ref: "#/definitions/UserInput"
                  },
                  type: "array"
                },
                id: {
                  type: "string"
                },
                type: {
                  enum: [
                    "userMessage"
                  ],
                  title: "UserMessageThreadItemType",
                  type: "string"
                }
              },
              required: [
                "content",
                "id",
                "type"
              ],
              title: "UserMessageThreadItem",
              type: "object"
            },
            {
              properties: {
                fragments: {
                  items: {
                    $ref: "#/definitions/HookPromptFragment"
                  },
                  type: "array"
                },
                id: {
                  type: "string"
                },
                type: {
                  enum: [
                    "hookPrompt"
                  ],
                  title: "HookPromptThreadItemType",
                  type: "string"
                }
              },
              required: [
                "fragments",
                "id",
                "type"
              ],
              title: "HookPromptThreadItem",
              type: "object"
            },
            {
              properties: {
                delivery: {
                  anyOf: [
                    {
                      $ref: "#/definitions/AgentMessageDelivery"
                    },
                    {
                      type: "null"
                    }
                  ],
                  default: null
                },
                id: {
                  type: "string"
                },
                memoryCitation: {
                  anyOf: [
                    {
                      $ref: "#/definitions/MemoryCitation"
                    },
                    {
                      type: "null"
                    }
                  ],
                  default: null
                },
                phase: {
                  anyOf: [
                    {
                      $ref: "#/definitions/MessagePhase"
                    },
                    {
                      type: "null"
                    }
                  ],
                  default: null
                },
                questions: {
                  default: null,
                  items: {
                    $ref: "#/definitions/AsyncUserInputQuestion"
                  },
                  type: [
                    "array",
                    "null"
                  ]
                },
                text: {
                  type: "string"
                },
                type: {
                  enum: [
                    "agentMessage"
                  ],
                  title: "AgentMessageThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "text",
                "type"
              ],
              title: "AgentMessageThreadItem",
              type: "object"
            },
            {
              properties: {
                id: {
                  type: "string"
                },
                name: {
                  type: "string"
                },
                namespace: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                output: {
                  $ref: "#/definitions/FunctionCallOutputBody"
                },
                type: {
                  enum: [
                    "functionCallOutput"
                  ],
                  title: "FunctionCallOutputThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "name",
                "output",
                "type"
              ],
              title: "FunctionCallOutputThreadItem",
              type: "object"
            },
            {
              description: "EXPERIMENTAL - proposed plan item content. The completed plan item is authoritative and may not match the concatenation of `PlanDelta` text.",
              properties: {
                id: {
                  type: "string"
                },
                text: {
                  type: "string"
                },
                type: {
                  enum: [
                    "plan"
                  ],
                  title: "PlanThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "text",
                "type"
              ],
              title: "PlanThreadItem",
              type: "object"
            },
            {
              properties: {
                content: {
                  default: [],
                  items: {
                    type: "string"
                  },
                  type: "array"
                },
                id: {
                  type: "string"
                },
                summary: {
                  default: [],
                  items: {
                    type: "string"
                  },
                  type: "array"
                },
                type: {
                  enum: [
                    "reasoning"
                  ],
                  title: "ReasoningThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "type"
              ],
              title: "ReasoningThreadItem",
              type: "object"
            },
            {
              properties: {
                aggregatedOutput: {
                  description: "The command's output, aggregated from stdout and stderr.",
                  type: [
                    "string",
                    "null"
                  ]
                },
                command: {
                  description: "The command to be executed.",
                  type: "string"
                },
                commandActions: {
                  description: "A best-effort parsing of the command to understand the action(s) it will perform. This returns a list of CommandAction objects because a single shell command may be composed of many commands piped together.",
                  items: {
                    $ref: "#/definitions/CommandAction"
                  },
                  type: "array"
                },
                cwd: {
                  allOf: [
                    {
                      $ref: "#/definitions/LegacyAppPathString"
                    }
                  ],
                  description: "The command's working directory."
                },
                durationMs: {
                  description: "The duration of the command execution in milliseconds.",
                  format: "int64",
                  type: [
                    "integer",
                    "null"
                  ]
                },
                exitCode: {
                  description: "The command's exit code.",
                  format: "int32",
                  type: [
                    "integer",
                    "null"
                  ]
                },
                id: {
                  type: "string"
                },
                pluginId: {
                  default: null,
                  description: "Trusted first-party plugin id when this command resolves to one plugin script.",
                  type: [
                    "string",
                    "null"
                  ]
                },
                processId: {
                  description: "Identifier for the underlying PTY process (when available).",
                  type: [
                    "string",
                    "null"
                  ]
                },
                scriptPath: {
                  default: null,
                  description: "Safe plugin-relative path when this command resolves to one plugin script.",
                  type: [
                    "string",
                    "null"
                  ]
                },
                source: {
                  allOf: [
                    {
                      $ref: "#/definitions/CommandExecutionSource"
                    }
                  ],
                  default: "agent"
                },
                status: {
                  $ref: "#/definitions/CommandExecutionStatus"
                },
                type: {
                  enum: [
                    "commandExecution"
                  ],
                  title: "CommandExecutionThreadItemType",
                  type: "string"
                }
              },
              required: [
                "command",
                "commandActions",
                "cwd",
                "id",
                "status",
                "type"
              ],
              title: "CommandExecutionThreadItem",
              type: "object"
            },
            {
              properties: {
                changes: {
                  items: {
                    $ref: "#/definitions/FileUpdateChange"
                  },
                  type: "array"
                },
                id: {
                  type: "string"
                },
                status: {
                  $ref: "#/definitions/PatchApplyStatus"
                },
                type: {
                  enum: [
                    "fileChange"
                  ],
                  title: "FileChangeThreadItemType",
                  type: "string"
                }
              },
              required: [
                "changes",
                "id",
                "status",
                "type"
              ],
              title: "FileChangeThreadItem",
              type: "object"
            },
            {
              properties: {
                appContext: {
                  anyOf: [
                    {
                      $ref: "#/definitions/McpToolCallAppContext"
                    },
                    {
                      type: "null"
                    }
                  ]
                },
                arguments: true,
                durationMs: {
                  description: "The duration of the MCP tool call in milliseconds.",
                  format: "int64",
                  type: [
                    "integer",
                    "null"
                  ]
                },
                error: {
                  anyOf: [
                    {
                      $ref: "#/definitions/McpToolCallError"
                    },
                    {
                      type: "null"
                    }
                  ]
                },
                id: {
                  type: "string"
                },
                mcpAppResourceUri: {
                  description: "Legacy compatibility field; prefer `mcpAppUi.resourceUri` when available.",
                  type: [
                    "string",
                    "null"
                  ]
                },
                mcpAppUi: {
                  anyOf: [
                    {
                      $ref: "#/definitions/McpAppUi"
                    },
                    {
                      type: "null"
                    }
                  ],
                  description: "Presentation captured from the invoked descriptor; absent in older history."
                },
                pluginId: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                readOnlyHint: {
                  type: [
                    "boolean",
                    "null"
                  ]
                },
                result: {
                  anyOf: [
                    {
                      $ref: "#/definitions/McpToolCallResult"
                    },
                    {
                      type: "null"
                    }
                  ]
                },
                server: {
                  type: "string"
                },
                status: {
                  $ref: "#/definitions/McpToolCallStatus"
                },
                tool: {
                  type: "string"
                },
                type: {
                  enum: [
                    "mcpToolCall"
                  ],
                  title: "McpToolCallThreadItemType",
                  type: "string"
                }
              },
              required: [
                "arguments",
                "id",
                "server",
                "status",
                "tool",
                "type"
              ],
              title: "McpToolCallThreadItem",
              type: "object"
            },
            {
              properties: {
                arguments: true,
                contentItems: {
                  items: {
                    $ref: "#/definitions/DynamicToolCallOutputContentItem"
                  },
                  type: [
                    "array",
                    "null"
                  ]
                },
                durationMs: {
                  description: "The duration of the dynamic tool call in milliseconds.",
                  format: "int64",
                  type: [
                    "integer",
                    "null"
                  ]
                },
                id: {
                  type: "string"
                },
                namespace: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                status: {
                  $ref: "#/definitions/DynamicToolCallStatus"
                },
                success: {
                  type: [
                    "boolean",
                    "null"
                  ]
                },
                tool: {
                  type: "string"
                },
                type: {
                  enum: [
                    "dynamicToolCall"
                  ],
                  title: "DynamicToolCallThreadItemType",
                  type: "string"
                }
              },
              required: [
                "arguments",
                "id",
                "status",
                "tool",
                "type"
              ],
              title: "DynamicToolCallThreadItem",
              type: "object"
            },
            {
              properties: {
                agentsStates: {
                  additionalProperties: {
                    $ref: "#/definitions/CollabAgentState"
                  },
                  description: "Last known status of the target agents, when available.",
                  type: "object"
                },
                id: {
                  description: "Unique identifier for this collab tool call.",
                  type: "string"
                },
                model: {
                  description: "Model requested for the spawned agent, when applicable.",
                  type: [
                    "string",
                    "null"
                  ]
                },
                prompt: {
                  description: "Prompt text sent as part of the collab tool call, when available.",
                  type: [
                    "string",
                    "null"
                  ]
                },
                reasoningEffort: {
                  anyOf: [
                    {
                      $ref: "#/definitions/ReasoningEffort"
                    },
                    {
                      type: "null"
                    }
                  ],
                  description: "Reasoning effort requested for the spawned agent, when applicable."
                },
                receiverThreadIds: {
                  description: "Thread ID of the receiving agent, when applicable. In case of spawn operation, this corresponds to the newly spawned agent.",
                  items: {
                    type: "string"
                  },
                  type: "array"
                },
                senderThreadId: {
                  description: "Thread ID of the agent issuing the collab request.",
                  type: "string"
                },
                status: {
                  allOf: [
                    {
                      $ref: "#/definitions/CollabAgentToolCallStatus"
                    }
                  ],
                  description: "Current status of the collab tool call."
                },
                tool: {
                  allOf: [
                    {
                      $ref: "#/definitions/CollabAgentTool"
                    }
                  ],
                  description: "Name of the collab tool that was invoked."
                },
                type: {
                  enum: [
                    "collabAgentToolCall"
                  ],
                  title: "CollabAgentToolCallThreadItemType",
                  type: "string"
                }
              },
              required: [
                "agentsStates",
                "id",
                "receiverThreadIds",
                "senderThreadId",
                "status",
                "tool",
                "type"
              ],
              title: "CollabAgentToolCallThreadItem",
              type: "object"
            },
            {
              properties: {
                agentPath: {
                  type: "string"
                },
                agentThreadId: {
                  type: "string"
                },
                id: {
                  type: "string"
                },
                kind: {
                  $ref: "#/definitions/SubAgentActivityKind"
                },
                model: {
                  description: "Resolved model at sub-agent creation; absent from older records and other activities.",
                  type: [
                    "string",
                    "null"
                  ]
                },
                reasoningEffort: {
                  anyOf: [
                    {
                      $ref: "#/definitions/ReasoningEffort"
                    },
                    {
                      type: "null"
                    }
                  ],
                  description: "Resolved reasoning effort at sub-agent creation, when known."
                },
                type: {
                  enum: [
                    "subAgentActivity"
                  ],
                  title: "SubAgentActivityThreadItemType",
                  type: "string"
                }
              },
              required: [
                "agentPath",
                "agentThreadId",
                "id",
                "kind",
                "type"
              ],
              title: "SubAgentActivityThreadItem",
              type: "object"
            },
            {
              properties: {
                action: {
                  anyOf: [
                    {
                      $ref: "#/definitions/WebSearchAction"
                    },
                    {
                      type: "null"
                    }
                  ]
                },
                id: {
                  type: "string"
                },
                query: {
                  type: "string"
                },
                results: {
                  default: null,
                  description: "Structured search results returned out-of-band by standalone web search.\n\nThese stay as opaque JSON at the extension/app-server boundary so new result fields and result types can pass through without a Codex release.",
                  items: true,
                  type: [
                    "array",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "webSearch"
                  ],
                  title: "WebSearchThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "query",
                "type"
              ],
              title: "WebSearchThreadItem",
              type: "object"
            },
            {
              properties: {
                id: {
                  type: "string"
                },
                path: {
                  $ref: "#/definitions/LegacyAppPathString"
                },
                type: {
                  enum: [
                    "imageView"
                  ],
                  title: "ImageViewThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "path",
                "type"
              ],
              title: "ImageViewThreadItem",
              type: "object"
            },
            {
              description: "Display item emitted by the interruptible `clock.sleep` tool.",
              properties: {
                durationMs: {
                  format: "uint64",
                  minimum: 0,
                  type: "integer"
                },
                id: {
                  type: "string"
                },
                type: {
                  enum: [
                    "sleep"
                  ],
                  title: "SleepThreadItemType",
                  type: "string"
                }
              },
              required: [
                "durationMs",
                "id",
                "type"
              ],
              title: "SleepThreadItem",
              type: "object"
            },
            {
              properties: {
                failure: {
                  anyOf: [
                    {
                      $ref: "#/definitions/ImageGenerationFailure"
                    },
                    {
                      type: "null"
                    }
                  ],
                  default: null
                },
                id: {
                  type: "string"
                },
                result: {
                  type: "string"
                },
                revisedPrompt: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                savedPath: {
                  anyOf: [
                    {
                      $ref: "#/definitions/AbsolutePathBuf"
                    },
                    {
                      type: "null"
                    }
                  ]
                },
                status: {
                  type: "string"
                },
                transparentBackground: {
                  default: null,
                  type: [
                    "boolean",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "imageGeneration"
                  ],
                  title: "ImageGenerationThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "result",
                "status",
                "type"
              ],
              title: "ImageGenerationThreadItem",
              type: "object"
            },
            {
              properties: {
                id: {
                  type: "string"
                },
                review: {
                  type: "string"
                },
                type: {
                  enum: [
                    "enteredReviewMode"
                  ],
                  title: "EnteredReviewModeThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "review",
                "type"
              ],
              title: "EnteredReviewModeThreadItem",
              type: "object"
            },
            {
              properties: {
                id: {
                  type: "string"
                },
                review: {
                  type: "string"
                },
                type: {
                  enum: [
                    "exitedReviewMode"
                  ],
                  title: "ExitedReviewModeThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "review",
                "type"
              ],
              title: "ExitedReviewModeThreadItem",
              type: "object"
            },
            {
              properties: {
                id: {
                  type: "string"
                },
                type: {
                  enum: [
                    "contextCompaction"
                  ],
                  title: "ContextCompactionThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "type"
              ],
              title: "ContextCompactionThreadItem",
              type: "object"
            }
          ]
        },
        ThreadNameUpdatedNotification: {
          properties: {
            threadId: {
              type: "string"
            },
            threadName: {
              type: [
                "string",
                "null"
              ]
            }
          },
          required: [
            "threadId"
          ],
          type: "object"
        },
        ThreadPredictionResult: {
          oneOf: [
            {
              properties: {
                text: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "completed"
                  ],
                  title: "CompletedThreadPredictionResultType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "CompletedThreadPredictionResult",
              type: "object"
            },
            {
              properties: {
                type: {
                  enum: [
                    "failed"
                  ],
                  title: "FailedThreadPredictionResultType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "FailedThreadPredictionResult",
              type: "object"
            }
          ]
        },
        ThreadPredictionUpdatedNotification: {
          properties: {
            result: {
              $ref: "#/definitions/ThreadPredictionResult"
            },
            sourceTurnId: {
              type: "string"
            },
            threadId: {
              type: "string"
            }
          },
          required: [
            "result",
            "sourceTurnId",
            "threadId"
          ],
          type: "object"
        },
        ThreadProjectUpdatedNotification: {
          properties: {
            projectId: {
              type: [
                "string",
                "null"
              ]
            },
            threadId: {
              type: "string"
            }
          },
          required: [
            "projectId",
            "threadId"
          ],
          type: "object"
        },
        ThreadQueueChangedNotification: {
          properties: {
            threadId: {
              type: "string"
            }
          },
          required: [
            "threadId"
          ],
          type: "object"
        },
        ThreadRealtimeAudioChunk: {
          description: "EXPERIMENTAL - thread realtime audio chunk.",
          properties: {
            data: {
              type: "string"
            },
            itemId: {
              type: [
                "string",
                "null"
              ]
            },
            numChannels: {
              format: "uint16",
              minimum: 0,
              type: "integer"
            },
            sampleRate: {
              format: "uint32",
              minimum: 0,
              type: "integer"
            },
            samplesPerChannel: {
              format: "uint32",
              minimum: 0,
              type: [
                "integer",
                "null"
              ]
            }
          },
          required: [
            "data",
            "numChannels",
            "sampleRate"
          ],
          type: "object"
        },
        ThreadRealtimeBemItemPresentation: {
          description: "EXPERIMENTAL - how an existing agent item appears in a realtime conversation.",
          oneOf: [
            {
              properties: {
                type: {
                  enum: [
                    "wholeItem"
                  ],
                  title: "WholeItemThreadRealtimeBemItemPresentationType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "WholeItemThreadRealtimeBemItemPresentation",
              type: "object"
            },
            {
              properties: {
                type: {
                  enum: [
                    "inlineMarkdown"
                  ],
                  title: "InlineMarkdownThreadRealtimeBemItemPresentationType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "InlineMarkdownThreadRealtimeBemItemPresentation",
              type: "object"
            },
            {
              properties: {
                index: {
                  format: "uint32",
                  minimum: 0,
                  type: "integer"
                },
                type: {
                  enum: [
                    "inlineVisualization"
                  ],
                  title: "InlineVisualizationThreadRealtimeBemItemPresentationType",
                  type: "string"
                }
              },
              required: [
                "index",
                "type"
              ],
              title: "InlineVisualizationThreadRealtimeBemItemPresentation",
              type: "object"
            }
          ]
        },
        ThreadRealtimeClosedNotification: {
          description: "EXPERIMENTAL - emitted when thread realtime transport closes.",
          properties: {
            reason: {
              type: [
                "string",
                "null"
              ]
            },
            threadId: {
              type: "string"
            }
          },
          required: [
            "threadId"
          ],
          type: "object"
        },
        ThreadRealtimeErrorNotification: {
          description: "EXPERIMENTAL - emitted when thread realtime encounters an error.",
          properties: {
            message: {
              type: "string"
            },
            threadId: {
              type: "string"
            }
          },
          required: [
            "message",
            "threadId"
          ],
          type: "object"
        },
        ThreadRealtimeItem: {
          description: "EXPERIMENTAL - a thread-scoped realtime item in the canonical timeline.",
          oneOf: [
            {
              properties: {
                type: {
                  enum: [
                    "realtimeSessionStarted"
                  ],
                  title: "RealtimeSessionStartedThreadRealtimeItemType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "RealtimeSessionStartedThreadRealtimeItem",
              type: "object"
            },
            {
              properties: {
                role: {
                  $ref: "#/definitions/ThreadRealtimeTranscriptRole"
                },
                text: {
                  type: "string"
                },
                type: {
                  enum: [
                    "transcriptSegment"
                  ],
                  title: "TranscriptSegmentThreadRealtimeItemType",
                  type: "string"
                }
              },
              required: [
                "role",
                "text",
                "type"
              ],
              title: "TranscriptSegmentThreadRealtimeItem",
              type: "object"
            },
            {
              properties: {
                item_id: {
                  type: "string"
                },
                presentation: {
                  $ref: "#/definitions/ThreadRealtimeBemItemPresentation"
                },
                turn_id: {
                  type: "string"
                },
                type: {
                  enum: [
                    "bemItemPromoted"
                  ],
                  title: "BemItemPromotedThreadRealtimeItemType",
                  type: "string"
                }
              },
              required: [
                "item_id",
                "presentation",
                "turn_id",
                "type"
              ],
              title: "BemItemPromotedThreadRealtimeItem",
              type: "object"
            },
            {
              properties: {
                outcome: {
                  $ref: "#/definitions/ThreadRealtimeSessionOutcome"
                },
                type: {
                  enum: [
                    "realtimeSessionClosed"
                  ],
                  title: "RealtimeSessionClosedThreadRealtimeItemType",
                  type: "string"
                }
              },
              required: [
                "outcome",
                "type"
              ],
              title: "RealtimeSessionClosedThreadRealtimeItem",
              type: "object"
            }
          ],
          properties: {
            id: {
              type: "string"
            },
            realtimeSessionId: {
              type: "string"
            }
          },
          required: [
            "id",
            "realtimeSessionId"
          ],
          type: "object"
        },
        ThreadRealtimeItemAddedNotification: {
          description: "EXPERIMENTAL - raw non-audio thread realtime item emitted by the backend.",
          properties: {
            item: true,
            threadId: {
              type: "string"
            }
          },
          required: [
            "item",
            "threadId"
          ],
          type: "object"
        },
        ThreadRealtimeItemCompletedNotification: {
          description: "EXPERIMENTAL - a realtime timeline item published after canonical commit.",
          properties: {
            item: {
              $ref: "#/definitions/ThreadRealtimeItem"
            },
            threadId: {
              type: "string"
            }
          },
          required: [
            "item",
            "threadId"
          ],
          type: "object"
        },
        ThreadRealtimeItemStartedNotification: {
          description: "EXPERIMENTAL - a realtime timeline item started before its content streams.",
          properties: {
            item: {
              $ref: "#/definitions/ThreadRealtimeItem"
            },
            threadId: {
              type: "string"
            }
          },
          required: [
            "item",
            "threadId"
          ],
          type: "object"
        },
        ThreadRealtimeItemTranscriptDeltaNotification: {
          description: "EXPERIMENTAL - text appended to an active realtime transcript item.",
          properties: {
            delta: {
              type: "string"
            },
            itemId: {
              type: "string"
            },
            threadId: {
              type: "string"
            }
          },
          required: [
            "delta",
            "itemId",
            "threadId"
          ],
          type: "object"
        },
        ThreadRealtimeOutputAudioDeltaNotification: {
          description: "EXPERIMENTAL - streamed output audio emitted by thread realtime.",
          properties: {
            audio: {
              $ref: "#/definitions/ThreadRealtimeAudioChunk"
            },
            threadId: {
              type: "string"
            }
          },
          required: [
            "audio",
            "threadId"
          ],
          type: "object"
        },
        ThreadRealtimeSdpNotification: {
          description: "EXPERIMENTAL - emitted with the remote SDP for a WebRTC realtime session.",
          properties: {
            sdp: {
              type: "string"
            },
            threadId: {
              type: "string"
            }
          },
          required: [
            "sdp",
            "threadId"
          ],
          type: "object"
        },
        ThreadRealtimeSessionOutcome: {
          enum: [
            "ended",
            "failed"
          ],
          type: "string"
        },
        ThreadRealtimeStartedNotification: {
          description: "EXPERIMENTAL - emitted when thread realtime startup is accepted.",
          properties: {
            realtimeSessionId: {
              type: [
                "string",
                "null"
              ]
            },
            threadId: {
              type: "string"
            },
            version: {
              $ref: "#/definitions/RealtimeConversationVersion"
            }
          },
          required: [
            "threadId",
            "version"
          ],
          type: "object"
        },
        ThreadRealtimeTranscriptDeltaNotification: {
          description: "EXPERIMENTAL - flat transcript delta emitted whenever realtime transcript text changes.",
          properties: {
            delta: {
              description: "Live transcript delta from the realtime event.",
              type: "string"
            },
            role: {
              type: "string"
            },
            threadId: {
              type: "string"
            }
          },
          required: [
            "delta",
            "role",
            "threadId"
          ],
          type: "object"
        },
        ThreadRealtimeTranscriptDoneNotification: {
          description: "EXPERIMENTAL - final transcript text emitted when realtime completes a transcript part.",
          properties: {
            role: {
              type: "string"
            },
            text: {
              description: "Final complete text for the transcript part.",
              type: "string"
            },
            threadId: {
              type: "string"
            }
          },
          required: [
            "role",
            "text",
            "threadId"
          ],
          type: "object"
        },
        ThreadRealtimeTranscriptRole: {
          enum: [
            "user",
            "assistant"
          ],
          type: "string"
        },
        ThreadRevertedNotification: {
          properties: {
            threadId: {
              type: "string"
            }
          },
          required: [
            "threadId"
          ],
          type: "object"
        },
        ThreadSection: {
          description: "An independently persisted, user-visible thread section.",
          properties: {
            appearance: {
              anyOf: [
                {
                  $ref: "#/definitions/ThreadSectionAppearance"
                },
                {
                  type: "null"
                }
              ],
              default: null,
              description: "Optional appearance synchronized across clients."
            },
            id: {
              description: "Opaque UUIDv7 identity that remains stable when the section is renamed.",
              type: "string"
            },
            name: {
              description: "The current user-visible section name.",
              type: "string"
            }
          },
          required: [
            "id",
            "name"
          ],
          type: "object"
        },
        ThreadSectionAppearance: {
          description: "Extensible visual presentation for a custom thread section.",
          properties: {
            color: {
              type: [
                "string",
                "null"
              ]
            },
            icon: {
              type: [
                "string",
                "null"
              ]
            }
          },
          type: "object"
        },
        ThreadSettings: {
          properties: {
            activePermissionProfile: {
              anyOf: [
                {
                  $ref: "#/definitions/ActivePermissionProfile"
                },
                {
                  type: "null"
                }
              ]
            },
            approvalPolicy: {
              $ref: "#/definitions/AskForApproval"
            },
            approvalsReviewer: {
              $ref: "#/definitions/ApprovalsReviewer"
            },
            collaborationMode: {
              $ref: "#/definitions/CollaborationMode"
            },
            cwd: {
              $ref: "#/definitions/AbsolutePathBuf"
            },
            disabledPluginIds: {
              default: [],
              description: "Saved list of disabled plugin IDs. Does not yet filter plugin capabilities.",
              items: {
                type: "string"
              },
              type: "array"
            },
            effort: {
              anyOf: [
                {
                  $ref: "#/definitions/ReasoningEffort"
                },
                {
                  type: "null"
                }
              ]
            },
            model: {
              type: "string"
            },
            modelProvider: {
              type: "string"
            },
            personality: {
              anyOf: [
                {
                  $ref: "#/definitions/Personality"
                },
                {
                  type: "null"
                }
              ],
              description: "@deprecated Reports the saved setting; `friendly` and `pragmatic` no longer select a style."
            },
            sandboxPolicy: {
              $ref: "#/definitions/SandboxPolicy"
            },
            serviceTier: {
              type: [
                "string",
                "null"
              ]
            },
            summary: {
              anyOf: [
                {
                  $ref: "#/definitions/ReasoningSummary"
                },
                {
                  type: "null"
                }
              ]
            }
          },
          required: [
            "approvalPolicy",
            "approvalsReviewer",
            "collaborationMode",
            "cwd",
            "model",
            "modelProvider",
            "sandboxPolicy"
          ],
          type: "object"
        },
        ThreadSettingsUpdatedNotification: {
          properties: {
            threadId: {
              type: "string"
            },
            threadSettings: {
              $ref: "#/definitions/ThreadSettings"
            }
          },
          required: [
            "threadId",
            "threadSettings"
          ],
          type: "object"
        },
        ThreadSource: {
          type: "string"
        },
        ThreadStartedNotification: {
          properties: {
            thread: {
              $ref: "#/definitions/Thread"
            }
          },
          required: [
            "thread"
          ],
          type: "object"
        },
        ThreadStatus: {
          oneOf: [
            {
              properties: {
                type: {
                  enum: [
                    "notLoaded"
                  ],
                  title: "NotLoadedThreadStatusType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "NotLoadedThreadStatus",
              type: "object"
            },
            {
              properties: {
                type: {
                  enum: [
                    "idle"
                  ],
                  title: "IdleThreadStatusType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "IdleThreadStatus",
              type: "object"
            },
            {
              properties: {
                type: {
                  enum: [
                    "systemError"
                  ],
                  title: "SystemErrorThreadStatusType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "SystemErrorThreadStatus",
              type: "object"
            },
            {
              properties: {
                activeFlags: {
                  items: {
                    $ref: "#/definitions/ThreadActiveFlag"
                  },
                  type: "array"
                },
                type: {
                  enum: [
                    "active"
                  ],
                  title: "ActiveThreadStatusType",
                  type: "string"
                }
              },
              required: [
                "activeFlags",
                "type"
              ],
              title: "ActiveThreadStatus",
              type: "object"
            }
          ]
        },
        ThreadStatusChangedNotification: {
          properties: {
            status: {
              $ref: "#/definitions/ThreadStatus"
            },
            threadId: {
              type: "string"
            }
          },
          required: [
            "status",
            "threadId"
          ],
          type: "object"
        },
        ThreadTokenUsage: {
          properties: {
            last: {
              $ref: "#/definitions/TokenUsageBreakdown"
            },
            modelContextWindow: {
              format: "int64",
              type: [
                "integer",
                "null"
              ]
            },
            total: {
              $ref: "#/definitions/TokenUsageBreakdown"
            }
          },
          required: [
            "last",
            "total"
          ],
          type: "object"
        },
        ThreadTokenUsageUpdatedNotification: {
          properties: {
            threadId: {
              type: "string"
            },
            tokenUsage: {
              $ref: "#/definitions/ThreadTokenUsage"
            },
            turnId: {
              type: "string"
            }
          },
          required: [
            "threadId",
            "tokenUsage",
            "turnId"
          ],
          type: "object"
        },
        ThreadUnarchivedNotification: {
          properties: {
            threadId: {
              type: "string"
            }
          },
          required: [
            "threadId"
          ],
          type: "object"
        },
        TokenUsageBreakdown: {
          properties: {
            cacheWriteInputTokens: {
              default: 0,
              format: "int64",
              type: "integer"
            },
            cachedInputTokens: {
              format: "int64",
              type: "integer"
            },
            inputTokens: {
              format: "int64",
              type: "integer"
            },
            outputTokens: {
              format: "int64",
              type: "integer"
            },
            reasoningOutputTokens: {
              format: "int64",
              type: "integer"
            },
            totalTokens: {
              format: "int64",
              type: "integer"
            }
          },
          required: [
            "cachedInputTokens",
            "inputTokens",
            "outputTokens",
            "reasoningOutputTokens",
            "totalTokens"
          ],
          type: "object"
        },
        Turn: {
          properties: {
            completedAt: {
              description: "Unix timestamp (in seconds) when the turn completed.",
              format: "int64",
              type: [
                "integer",
                "null"
              ]
            },
            durationMs: {
              description: "Duration between turn start and completion in milliseconds, if known.",
              format: "int64",
              type: [
                "integer",
                "null"
              ]
            },
            error: {
              anyOf: [
                {
                  $ref: "#/definitions/TurnError"
                },
                {
                  type: "null"
                }
              ],
              description: "Error associated with a failed or interrupted turn."
            },
            id: {
              description: "Identifier for this turn. Codex-generated turn IDs are UUIDv7.",
              type: "string"
            },
            items: {
              description: "Thread items currently included in this turn payload.",
              items: {
                $ref: "#/definitions/ThreadItem"
              },
              type: "array"
            },
            itemsView: {
              allOf: [
                {
                  $ref: "#/definitions/TurnItemsView"
                }
              ],
              default: "full",
              description: "Describes how much of `items` has been loaded for this turn."
            },
            rootTurnId: {
              description: "ID of the first turn in the chain of work that led to this turn. Pass this as `rootTurnId` when starting work on behalf of this turn. May be null in older history or a `review/start` response.",
              type: [
                "string",
                "null"
              ]
            },
            startedAt: {
              description: "Unix timestamp (in seconds) when the turn started.",
              format: "int64",
              type: [
                "integer",
                "null"
              ]
            },
            status: {
              $ref: "#/definitions/TurnStatus"
            }
          },
          required: [
            "id",
            "items",
            "status"
          ],
          type: "object"
        },
        TurnCompletedNotification: {
          properties: {
            threadId: {
              type: "string"
            },
            turn: {
              $ref: "#/definitions/Turn"
            }
          },
          required: [
            "threadId",
            "turn"
          ],
          type: "object"
        },
        TurnDiffUpdatedNotification: {
          description: "Notification that the turn-level unified diff has changed. Contains the latest aggregated diff across all file changes in the turn.",
          properties: {
            diff: {
              type: "string"
            },
            threadId: {
              type: "string"
            },
            turnId: {
              type: "string"
            }
          },
          required: [
            "diff",
            "threadId",
            "turnId"
          ],
          type: "object"
        },
        TurnError: {
          properties: {
            additionalDetails: {
              default: null,
              type: [
                "string",
                "null"
              ]
            },
            codexErrorInfo: {
              anyOf: [
                {
                  $ref: "#/definitions/CodexErrorInfo"
                },
                {
                  type: "null"
                }
              ]
            },
            message: {
              type: "string"
            },
            misalignment: {
              anyOf: [
                {
                  $ref: "#/definitions/MisalignmentErrorDetails"
                },
                {
                  type: "null"
                }
              ],
              default: null,
              description: "Optional public explanation and continuation instruction for a misalignment block."
            }
          },
          required: [
            "message"
          ],
          type: "object"
        },
        TurnItemsView: {
          oneOf: [
            {
              description: "`items` was not loaded for this turn. The field is intentionally empty.",
              enum: [
                "notLoaded"
              ],
              type: "string"
            },
            {
              description: "`items` contains only a display summary for this turn.",
              enum: [
                "summary"
              ],
              type: "string"
            },
            {
              description: "`items` contains every ThreadItem available from persisted app-server history for this turn.",
              enum: [
                "full"
              ],
              type: "string"
            }
          ]
        },
        TurnModerationMetadataNotification: {
          properties: {
            metadata: true,
            threadId: {
              type: "string"
            },
            turnId: {
              type: "string"
            }
          },
          required: [
            "metadata",
            "threadId",
            "turnId"
          ],
          type: "object"
        },
        TurnPlanStep: {
          properties: {
            status: {
              $ref: "#/definitions/TurnPlanStepStatus"
            },
            step: {
              type: "string"
            }
          },
          required: [
            "status",
            "step"
          ],
          type: "object"
        },
        TurnPlanStepStatus: {
          enum: [
            "pending",
            "inProgress",
            "completed"
          ],
          type: "string"
        },
        TurnPlanUpdatedNotification: {
          properties: {
            explanation: {
              type: [
                "string",
                "null"
              ]
            },
            plan: {
              items: {
                $ref: "#/definitions/TurnPlanStep"
              },
              type: "array"
            },
            threadId: {
              type: "string"
            },
            turnId: {
              type: "string"
            }
          },
          required: [
            "plan",
            "threadId",
            "turnId"
          ],
          type: "object"
        },
        TurnStartedNotification: {
          properties: {
            threadId: {
              type: "string"
            },
            turn: {
              $ref: "#/definitions/Turn"
            }
          },
          required: [
            "threadId",
            "turn"
          ],
          type: "object"
        },
        TurnStatus: {
          enum: [
            "completed",
            "interrupted",
            "failed",
            "inProgress"
          ],
          type: "string"
        },
        UserInput: {
          oneOf: [
            {
              properties: {
                text: {
                  type: "string"
                },
                text_elements: {
                  default: [],
                  description: "UI-defined spans within `text` used to render or persist special elements.",
                  items: {
                    $ref: "#/definitions/TextElement"
                  },
                  type: "array"
                },
                type: {
                  enum: [
                    "text"
                  ],
                  title: "TextUserInputType",
                  type: "string"
                }
              },
              required: [
                "text",
                "type"
              ],
              title: "TextUserInput",
              type: "object"
            },
            {
              anyOf: [
                {
                  properties: {
                    url: {
                      type: "string"
                    }
                  },
                  required: [
                    "url"
                  ],
                  title: "UrlUserInput",
                  type: "object"
                },
                {
                  properties: {
                    fileId: {
                      type: "string"
                    }
                  },
                  required: [
                    "fileId"
                  ],
                  title: "FileIdUserInput",
                  type: "object"
                }
              ],
              properties: {
                detail: {
                  anyOf: [
                    {
                      $ref: "#/definitions/ImageDetail"
                    },
                    {
                      type: "null"
                    }
                  ],
                  default: null
                },
                type: {
                  enum: [
                    "image"
                  ],
                  title: "ImageUserInputType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "ImageUserInput",
              type: "object"
            },
            {
              properties: {
                detail: {
                  anyOf: [
                    {
                      $ref: "#/definitions/ImageDetail"
                    },
                    {
                      type: "null"
                    }
                  ],
                  default: null
                },
                path: {
                  type: "string"
                },
                type: {
                  enum: [
                    "localImage"
                  ],
                  title: "LocalImageUserInputType",
                  type: "string"
                }
              },
              required: [
                "path",
                "type"
              ],
              title: "LocalImageUserInput",
              type: "object"
            },
            {
              properties: {
                type: {
                  enum: [
                    "audio"
                  ],
                  title: "AudioUserInputType",
                  type: "string"
                },
                url: {
                  type: "string"
                }
              },
              required: [
                "type",
                "url"
              ],
              title: "AudioUserInput",
              type: "object"
            },
            {
              properties: {
                path: {
                  type: "string"
                },
                type: {
                  enum: [
                    "localAudio"
                  ],
                  title: "LocalAudioUserInputType",
                  type: "string"
                }
              },
              required: [
                "path",
                "type"
              ],
              title: "LocalAudioUserInput",
              type: "object"
            },
            {
              properties: {
                name: {
                  type: "string"
                },
                path: {
                  type: "string"
                },
                type: {
                  enum: [
                    "skill"
                  ],
                  title: "SkillUserInputType",
                  type: "string"
                }
              },
              required: [
                "name",
                "path",
                "type"
              ],
              title: "SkillUserInput",
              type: "object"
            },
            {
              properties: {
                name: {
                  type: "string"
                },
                path: {
                  type: "string"
                },
                type: {
                  enum: [
                    "mention"
                  ],
                  title: "MentionUserInputType",
                  type: "string"
                }
              },
              required: [
                "name",
                "path",
                "type"
              ],
              title: "MentionUserInput",
              type: "object"
            }
          ]
        },
        WarningNotification: {
          properties: {
            message: {
              description: "Concise warning message for the user.",
              type: "string"
            },
            threadId: {
              description: "Optional thread target when the warning applies to a specific thread.",
              type: [
                "string",
                "null"
              ]
            }
          },
          required: [
            "message"
          ],
          type: "object"
        },
        WebSearchAction: {
          oneOf: [
            {
              properties: {
                queries: {
                  items: {
                    type: "string"
                  },
                  type: [
                    "array",
                    "null"
                  ]
                },
                query: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "search"
                  ],
                  title: "SearchWebSearchActionType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "SearchWebSearchAction",
              type: "object"
            },
            {
              properties: {
                type: {
                  enum: [
                    "openPage"
                  ],
                  title: "OpenPageWebSearchActionType",
                  type: "string"
                },
                url: {
                  type: [
                    "string",
                    "null"
                  ]
                }
              },
              required: [
                "type"
              ],
              title: "OpenPageWebSearchAction",
              type: "object"
            },
            {
              properties: {
                pattern: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "findInPage"
                  ],
                  title: "FindInPageWebSearchActionType",
                  type: "string"
                },
                url: {
                  type: [
                    "string",
                    "null"
                  ]
                }
              },
              required: [
                "type"
              ],
              title: "FindInPageWebSearchAction",
              type: "object"
            },
            {
              properties: {
                type: {
                  enum: [
                    "other"
                  ],
                  title: "OtherWebSearchActionType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "OtherWebSearchAction",
              type: "object"
            }
          ]
        },
        WindowsSandboxSetupCompletedNotification: {
          properties: {
            error: {
              type: [
                "string",
                "null"
              ]
            },
            mode: {
              $ref: "#/definitions/WindowsSandboxSetupMode"
            },
            success: {
              type: "boolean"
            }
          },
          required: [
            "mode",
            "success"
          ],
          type: "object"
        },
        WindowsSandboxSetupMode: {
          enum: [
            "elevated",
            "unelevated"
          ],
          type: "string"
        },
        WindowsWorldWritableWarningNotification: {
          properties: {
            extraCount: {
              format: "uint",
              minimum: 0,
              type: "integer"
            },
            failedScan: {
              type: "boolean"
            },
            samplePaths: {
              items: {
                type: "string"
              },
              type: "array"
            }
          },
          required: [
            "extraCount",
            "failedScan",
            "samplePaths"
          ],
          type: "object"
        }
      },
      description: "Notification sent from the server to the client.",
      oneOf: [
        {
          description: "NEW NOTIFICATIONS",
          properties: {
            method: {
              enum: [
                "error"
              ],
              title: "ErrorNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ErrorNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "ErrorNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "thread/started"
              ],
              title: "Thread/startedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ThreadStartedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Thread/startedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "thread/status/changed"
              ],
              title: "Thread/status/changedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ThreadStatusChangedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Thread/status/changedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "thread/archived"
              ],
              title: "Thread/archivedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ThreadArchivedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Thread/archivedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "thread/deleted"
              ],
              title: "Thread/deletedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ThreadDeletedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Thread/deletedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "thread/unarchived"
              ],
              title: "Thread/unarchivedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ThreadUnarchivedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Thread/unarchivedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "thread/closed"
              ],
              title: "Thread/closedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ThreadClosedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Thread/closedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "thread/reverted"
              ],
              title: "Thread/revertedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ThreadRevertedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Thread/revertedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "skills/changed"
              ],
              title: "Skills/changedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/SkillsChangedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Skills/changedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "thread/name/updated"
              ],
              title: "Thread/name/updatedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ThreadNameUpdatedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Thread/name/updatedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "thread/attachment/updated"
              ],
              title: "Thread/attachment/updatedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ThreadAttachmentUpdatedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Thread/attachment/updatedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "thread/goal/updated"
              ],
              title: "Thread/goal/updatedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ThreadGoalUpdatedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Thread/goal/updatedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "thread/prediction/updated"
              ],
              title: "Thread/prediction/updatedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ThreadPredictionUpdatedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Thread/prediction/updatedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "thread/goal/cleared"
              ],
              title: "Thread/goal/clearedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ThreadGoalClearedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Thread/goal/clearedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "thread/queue/changed"
              ],
              title: "Thread/queue/changedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ThreadQueueChangedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Thread/queue/changedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "project/changed"
              ],
              title: "Project/changedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ProjectChangedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Project/changedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "thread/project/updated"
              ],
              title: "Thread/project/updatedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ThreadProjectUpdatedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Thread/project/updatedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "thread/environment/connected"
              ],
              title: "Thread/environment/connectedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/EnvironmentConnectionNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Thread/environment/connectedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "thread/environment/disconnected"
              ],
              title: "Thread/environment/disconnectedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/EnvironmentConnectionNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Thread/environment/disconnectedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "thread/settings/updated"
              ],
              title: "Thread/settings/updatedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ThreadSettingsUpdatedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Thread/settings/updatedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "thread/tokenUsage/updated"
              ],
              title: "Thread/tokenUsage/updatedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ThreadTokenUsageUpdatedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Thread/tokenUsage/updatedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "turn/started"
              ],
              title: "Turn/startedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/TurnStartedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Turn/startedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "hook/started"
              ],
              title: "Hook/startedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/HookStartedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Hook/startedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "turn/completed"
              ],
              title: "Turn/completedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/TurnCompletedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Turn/completedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "hook/completed"
              ],
              title: "Hook/completedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/HookCompletedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Hook/completedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "turn/diff/updated"
              ],
              title: "Turn/diff/updatedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/TurnDiffUpdatedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Turn/diff/updatedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "turn/plan/updated"
              ],
              title: "Turn/plan/updatedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/TurnPlanUpdatedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Turn/plan/updatedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "item/started"
              ],
              title: "Item/startedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ItemStartedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Item/startedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "item/autoApprovalReview/started"
              ],
              title: "Item/autoApprovalReview/startedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ItemGuardianApprovalReviewStartedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Item/autoApprovalReview/startedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "item/autoApprovalReview/completed"
              ],
              title: "Item/autoApprovalReview/completedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ItemGuardianApprovalReviewCompletedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Item/autoApprovalReview/completedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "autoApprovalReview/strictReviewRequired"
              ],
              title: "AutoApprovalReview/strictReviewRequiredNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/StrictReviewRequiredNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "AutoApprovalReview/strictReviewRequiredNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "item/completed"
              ],
              title: "Item/completedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ItemCompletedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Item/completedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "item/agentMessage/delta"
              ],
              title: "Item/agentMessage/deltaNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/AgentMessageDeltaNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Item/agentMessage/deltaNotification",
          type: "object"
        },
        {
          description: "EXPERIMENTAL - proposed plan streaming deltas for plan items.",
          properties: {
            method: {
              enum: [
                "item/plan/delta"
              ],
              title: "Item/plan/deltaNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/PlanDeltaNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Item/plan/deltaNotification",
          type: "object"
        },
        {
          description: "Stream base64-encoded stdout/stderr chunks for a running `command/exec` session.",
          properties: {
            method: {
              enum: [
                "command/exec/outputDelta"
              ],
              title: "Command/exec/outputDeltaNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/CommandExecOutputDeltaNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Command/exec/outputDeltaNotification",
          type: "object"
        },
        {
          description: "Stream base64-encoded stdout/stderr chunks for a running `process/spawn` session.",
          properties: {
            method: {
              enum: [
                "process/outputDelta"
              ],
              title: "Process/outputDeltaNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ProcessOutputDeltaNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Process/outputDeltaNotification",
          type: "object"
        },
        {
          description: "Final exit notification for a `process/spawn` session.",
          properties: {
            method: {
              enum: [
                "process/exited"
              ],
              title: "Process/exitedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ProcessExitedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Process/exitedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "item/commandExecution/outputDelta"
              ],
              title: "Item/commandExecution/outputDeltaNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/CommandExecutionOutputDeltaNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Item/commandExecution/outputDeltaNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "item/commandExecution/terminalInteraction"
              ],
              title: "Item/commandExecution/terminalInteractionNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/TerminalInteractionNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Item/commandExecution/terminalInteractionNotification",
          type: "object"
        },
        {
          description: "Deprecated legacy apply_patch output stream notification.",
          properties: {
            method: {
              enum: [
                "item/fileChange/outputDelta"
              ],
              title: "Item/fileChange/outputDeltaNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/FileChangeOutputDeltaNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Item/fileChange/outputDeltaNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "item/fileChange/patchUpdated"
              ],
              title: "Item/fileChange/patchUpdatedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/FileChangePatchUpdatedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Item/fileChange/patchUpdatedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "serverRequest/resolved"
              ],
              title: "ServerRequest/resolvedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ServerRequestResolvedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "ServerRequest/resolvedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "item/mcpToolCall/progress"
              ],
              title: "Item/mcpToolCall/progressNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/McpToolCallProgressNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Item/mcpToolCall/progressNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "mcpServer/oauthLogin/completed"
              ],
              title: "McpServer/oauthLogin/completedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/McpServerOauthLoginCompletedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "McpServer/oauthLogin/completedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "mcpServer/startupStatus/updated"
              ],
              title: "McpServer/startupStatus/updatedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/McpServerStatusUpdatedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "McpServer/startupStatus/updatedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "mcpServer/event/stream/notification"
              ],
              title: "McpServer/event/stream/notificationNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/McpServerEventStreamNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "McpServer/event/stream/notificationNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "account/updated"
              ],
              title: "Account/updatedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/AccountUpdatedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Account/updatedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "account/gatewayOAuth/changed"
              ],
              title: "Account/gatewayOAuth/changedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/GatewayOAuthChangedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Account/gatewayOAuth/changedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "account/rateLimits/updated"
              ],
              title: "Account/rateLimits/updatedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/AccountRateLimitsUpdatedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Account/rateLimits/updatedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "app/list/updated"
              ],
              title: "App/list/updatedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/AppListUpdatedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "App/list/updatedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "remoteControl/status/changed"
              ],
              title: "RemoteControl/status/changedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/RemoteControlStatusChangedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "RemoteControl/status/changedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "externalAgentConfig/import/progress"
              ],
              title: "ExternalAgentConfig/import/progressNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ExternalAgentConfigImportProgressNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "ExternalAgentConfig/import/progressNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "externalAgentConfig/import/completed"
              ],
              title: "ExternalAgentConfig/import/completedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ExternalAgentConfigImportCompletedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "ExternalAgentConfig/import/completedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "fs/changed"
              ],
              title: "Fs/changedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/FsChangedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Fs/changedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "item/reasoning/summaryTextDelta"
              ],
              title: "Item/reasoning/summaryTextDeltaNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ReasoningSummaryTextDeltaNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Item/reasoning/summaryTextDeltaNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "item/reasoning/summaryPartAdded"
              ],
              title: "Item/reasoning/summaryPartAddedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ReasoningSummaryPartAddedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Item/reasoning/summaryPartAddedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "item/reasoning/textDelta"
              ],
              title: "Item/reasoning/textDeltaNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ReasoningTextDeltaNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Item/reasoning/textDeltaNotification",
          type: "object"
        },
        {
          description: "Deprecated: Use `ContextCompaction` item type instead.",
          properties: {
            method: {
              enum: [
                "thread/compacted"
              ],
              title: "Thread/compactedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ContextCompactedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Thread/compactedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "model/rerouted"
              ],
              title: "Model/reroutedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ModelReroutedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Model/reroutedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "model/verification"
              ],
              title: "Model/verificationNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ModelVerificationNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Model/verificationNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "modelProvider/authRecoveryStarted"
              ],
              title: "ModelProvider/authRecoveryStartedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/AuthRecoveryNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "ModelProvider/authRecoveryStartedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "modelProvider/authRecoveryCompleted"
              ],
              title: "ModelProvider/authRecoveryCompletedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/AuthRecoveryNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "ModelProvider/authRecoveryCompletedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "turn/moderationMetadata"
              ],
              title: "Turn/moderationMetadataNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/TurnModerationMetadataNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Turn/moderationMetadataNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "model/safetyBuffering/updated"
              ],
              title: "Model/safetyBuffering/updatedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ModelSafetyBufferingUpdatedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Model/safetyBuffering/updatedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "warning"
              ],
              title: "WarningNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/WarningNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "WarningNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "guardianWarning"
              ],
              title: "GuardianWarningNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/GuardianWarningNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "GuardianWarningNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "deprecationNotice"
              ],
              title: "DeprecationNoticeNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/DeprecationNoticeNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "DeprecationNoticeNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "configWarning"
              ],
              title: "ConfigWarningNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ConfigWarningNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "ConfigWarningNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "fuzzyFileSearch/sessionUpdated"
              ],
              title: "FuzzyFileSearch/sessionUpdatedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/FuzzyFileSearchSessionUpdatedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "FuzzyFileSearch/sessionUpdatedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "fuzzyFileSearch/sessionCompleted"
              ],
              title: "FuzzyFileSearch/sessionCompletedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/FuzzyFileSearchSessionCompletedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "FuzzyFileSearch/sessionCompletedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "thread/realtime/started"
              ],
              title: "Thread/realtime/startedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ThreadRealtimeStartedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Thread/realtime/startedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "thread/realtime/itemAdded"
              ],
              title: "Thread/realtime/itemAddedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ThreadRealtimeItemAddedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Thread/realtime/itemAddedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "thread/realtime/item/started"
              ],
              title: "Thread/realtime/item/startedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ThreadRealtimeItemStartedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Thread/realtime/item/startedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "thread/realtime/item/transcript/delta"
              ],
              title: "Thread/realtime/item/transcript/deltaNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ThreadRealtimeItemTranscriptDeltaNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Thread/realtime/item/transcript/deltaNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "thread/realtime/item/completed"
              ],
              title: "Thread/realtime/item/completedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ThreadRealtimeItemCompletedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Thread/realtime/item/completedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "thread/realtime/transcript/delta"
              ],
              title: "Thread/realtime/transcript/deltaNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ThreadRealtimeTranscriptDeltaNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Thread/realtime/transcript/deltaNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "thread/realtime/transcript/done"
              ],
              title: "Thread/realtime/transcript/doneNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ThreadRealtimeTranscriptDoneNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Thread/realtime/transcript/doneNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "thread/realtime/outputAudio/delta"
              ],
              title: "Thread/realtime/outputAudio/deltaNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ThreadRealtimeOutputAudioDeltaNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Thread/realtime/outputAudio/deltaNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "thread/realtime/sdp"
              ],
              title: "Thread/realtime/sdpNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ThreadRealtimeSdpNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Thread/realtime/sdpNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "thread/realtime/error"
              ],
              title: "Thread/realtime/errorNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ThreadRealtimeErrorNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Thread/realtime/errorNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "thread/realtime/closed"
              ],
              title: "Thread/realtime/closedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/ThreadRealtimeClosedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Thread/realtime/closedNotification",
          type: "object"
        },
        {
          description: "Notifies the user of world-writable directories on Windows, which cannot be protected by the sandbox.",
          properties: {
            method: {
              enum: [
                "windows/worldWritableWarning"
              ],
              title: "Windows/worldWritableWarningNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/WindowsWorldWritableWarningNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Windows/worldWritableWarningNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "windowsSandbox/setupCompleted"
              ],
              title: "WindowsSandbox/setupCompletedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/WindowsSandboxSetupCompletedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "WindowsSandbox/setupCompletedNotification",
          type: "object"
        },
        {
          properties: {
            method: {
              enum: [
                "account/login/completed"
              ],
              title: "Account/login/completedNotificationMethod",
              type: "string"
            },
            params: {
              $ref: "#/definitions/AccountLoginCompletedNotification"
            }
          },
          required: [
            "method",
            "params"
          ],
          title: "Account/login/completedNotification",
          type: "object"
        }
      ],
      properties: {
        emittedAtMs: {
          description: "Unix timestamp (in milliseconds) when app-server emitted this notification.",
          format: "int64",
          type: "integer"
        }
      },
      title: "ServerNotification"
    };
  }
});

// src/codex-protocol/schema/v1/InitializeResponse.json
var InitializeResponse_default;
var init_InitializeResponse = __esm({
  "src/codex-protocol/schema/v1/InitializeResponse.json"() {
    InitializeResponse_default = {
      $schema: "http://json-schema.org/draft-07/schema#",
      definitions: {
        AbsolutePathBuf: {
          description: "A path that is guaranteed to be absolute and normalized (though it is not guaranteed to be canonicalized or exist on the filesystem).\n\nIMPORTANT: When deserializing an `AbsolutePathBuf`, a base path must be set using [AbsolutePathBufGuard::new]. If no base path is set, the deserialization will fail unless the path being deserialized is already absolute.",
          type: "string"
        }
      },
      properties: {
        codexHome: {
          allOf: [
            {
              $ref: "#/definitions/AbsolutePathBuf"
            }
          ],
          description: "Absolute path to the server's $CODEX_HOME directory."
        },
        platformFamily: {
          description: 'Platform family for the running app-server target, for example `"unix"` or `"windows"`.',
          type: "string"
        },
        platformOs: {
          description: 'Operating system for the running app-server target, for example `"macos"`, `"linux"`, or `"windows"`.',
          type: "string"
        },
        userAgent: {
          type: "string"
        }
      },
      required: [
        "codexHome",
        "platformFamily",
        "platformOs",
        "userAgent"
      ],
      title: "InitializeResponse",
      type: "object"
    };
  }
});

// src/codex-protocol/schema/v2/ThreadStartResponse.json
var ThreadStartResponse_default;
var init_ThreadStartResponse = __esm({
  "src/codex-protocol/schema/v2/ThreadStartResponse.json"() {
    ThreadStartResponse_default = {
      $schema: "http://json-schema.org/draft-07/schema#",
      definitions: {
        AbsolutePathBuf: {
          description: "A path that is guaranteed to be absolute and normalized (though it is not guaranteed to be canonicalized or exist on the filesystem).\n\nIMPORTANT: When deserializing an `AbsolutePathBuf`, a base path must be set using [AbsolutePathBufGuard::new]. If no base path is set, the deserialization will fail unless the path being deserialized is already absolute.",
          type: "string"
        },
        ActivePermissionProfile: {
          properties: {
            extends: {
              default: null,
              description: "Parent profile identifier from the selected permissions profile's `extends` setting, when present.",
              type: [
                "string",
                "null"
              ]
            },
            id: {
              description: "Identifier from `default_permissions` or the implicit built-in default, such as `:workspace` or a user-defined `[permissions.<id>]` profile.",
              type: "string"
            }
          },
          required: [
            "id"
          ],
          type: "object"
        },
        AgentMessageDelivery: {
          enum: [
            "async"
          ],
          type: "string"
        },
        AgentPath: {
          type: "string"
        },
        ApprovalsReviewer: {
          description: "Configures who approval requests are routed to for review. Examples include sandbox escapes, blocked network access, MCP approval prompts, and ARC escalations. Defaults to `user`. `auto_review` uses a carefully prompted subagent to gather relevant context and apply a risk-based decision framework before approving or denying the request. The legacy value `guardian_subagent` is accepted for compatibility.",
          enum: [
            "user",
            "auto_review",
            "guardian_subagent"
          ],
          type: "string"
        },
        AskForApproval: {
          oneOf: [
            {
              enum: [
                "untrusted",
                "on-request",
                "never"
              ],
              type: "string"
            },
            {
              additionalProperties: false,
              properties: {
                granular: {
                  properties: {
                    mcp_elicitations: {
                      type: "boolean"
                    },
                    request_permissions: {
                      default: false,
                      type: "boolean"
                    },
                    rules: {
                      type: "boolean"
                    },
                    sandbox_approval: {
                      type: "boolean"
                    },
                    skill_approval: {
                      default: false,
                      type: "boolean"
                    }
                  },
                  required: [
                    "mcp_elicitations",
                    "rules",
                    "sandbox_approval"
                  ],
                  type: "object"
                }
              },
              required: [
                "granular"
              ],
              title: "GranularAskForApproval",
              type: "object"
            }
          ]
        },
        AsyncUserInputQuestion: {
          additionalProperties: false,
          properties: {
            options: {
              items: {
                type: "string"
              },
              type: [
                "array",
                "null"
              ]
            },
            title: {
              type: "string"
            }
          },
          required: [
            "title"
          ],
          type: "object"
        },
        ByteRange: {
          properties: {
            end: {
              format: "uint",
              minimum: 0,
              type: "integer"
            },
            start: {
              format: "uint",
              minimum: 0,
              type: "integer"
            }
          },
          required: [
            "end",
            "start"
          ],
          type: "object"
        },
        CodexErrorInfo: {
          anyOf: [
            {
              enum: [
                "contextWindowExceeded",
                "sessionBudgetExceeded",
                "usageLimitExceeded",
                "rateLimitExceeded",
                "flexUnavailable",
                "serverOverloaded",
                "cyberPolicy",
                "misalignmentPolicyViolation",
                "tooManyDenials",
                "internalServerError",
                "unauthorized",
                "badRequest",
                "threadRollbackFailed",
                "sandboxError",
                "other"
              ],
              type: "string"
            },
            {
              additionalProperties: false,
              properties: {
                httpConnectionFailed: {
                  properties: {
                    httpStatusCode: {
                      format: "uint16",
                      minimum: 0,
                      type: [
                        "integer",
                        "null"
                      ]
                    }
                  },
                  type: "object"
                }
              },
              required: [
                "httpConnectionFailed"
              ],
              title: "HttpConnectionFailedCodexErrorInfo",
              type: "object"
            },
            {
              additionalProperties: false,
              description: "Failed to connect to the response SSE stream.",
              properties: {
                responseStreamConnectionFailed: {
                  properties: {
                    httpStatusCode: {
                      format: "uint16",
                      minimum: 0,
                      type: [
                        "integer",
                        "null"
                      ]
                    }
                  },
                  type: "object"
                }
              },
              required: [
                "responseStreamConnectionFailed"
              ],
              title: "ResponseStreamConnectionFailedCodexErrorInfo",
              type: "object"
            },
            {
              additionalProperties: false,
              description: "The response SSE stream disconnected in the middle of a turn before completion.",
              properties: {
                responseStreamDisconnected: {
                  properties: {
                    httpStatusCode: {
                      format: "uint16",
                      minimum: 0,
                      type: [
                        "integer",
                        "null"
                      ]
                    }
                  },
                  type: "object"
                }
              },
              required: [
                "responseStreamDisconnected"
              ],
              title: "ResponseStreamDisconnectedCodexErrorInfo",
              type: "object"
            },
            {
              additionalProperties: false,
              description: "Reached the retry limit for responses.",
              properties: {
                responseTooManyFailedAttempts: {
                  properties: {
                    httpStatusCode: {
                      format: "uint16",
                      minimum: 0,
                      type: [
                        "integer",
                        "null"
                      ]
                    }
                  },
                  type: "object"
                }
              },
              required: [
                "responseTooManyFailedAttempts"
              ],
              title: "ResponseTooManyFailedAttemptsCodexErrorInfo",
              type: "object"
            },
            {
              additionalProperties: false,
              description: "Returned when `turn/start` or `turn/steer` is submitted while the current active turn cannot accept same-turn steering, for example `/review` or manual `/compact`.",
              properties: {
                activeTurnNotSteerable: {
                  properties: {
                    turnKind: {
                      $ref: "#/definitions/NonSteerableTurnKind"
                    }
                  },
                  required: [
                    "turnKind"
                  ],
                  type: "object"
                }
              },
              required: [
                "activeTurnNotSteerable"
              ],
              title: "ActiveTurnNotSteerableCodexErrorInfo",
              type: "object"
            },
            {
              type: [
                "string",
                "object"
              ]
            }
          ],
          description: "This translation layer make sure that we expose codex error code in camel case.\n\nWhen an upstream HTTP status is available (for example, from the Responses API or a provider), it is forwarded in `httpStatusCode` on the relevant `codexErrorInfo` variant."
        },
        CollabAgentState: {
          properties: {
            message: {
              type: [
                "string",
                "null"
              ]
            },
            status: {
              $ref: "#/definitions/CollabAgentStatus"
            }
          },
          required: [
            "status"
          ],
          type: "object"
        },
        CollabAgentStatus: {
          enum: [
            "pendingInit",
            "running",
            "interrupted",
            "completed",
            "errored",
            "shutdown",
            "notFound"
          ],
          type: "string"
        },
        CollabAgentTool: {
          enum: [
            "spawnAgent",
            "sendInput",
            "resumeAgent",
            "wait",
            "closeAgent",
            "sendMessage",
            "followupTask",
            "interruptAgent",
            "listAgents"
          ],
          type: "string"
        },
        CollabAgentToolCallStatus: {
          enum: [
            "inProgress",
            "completed",
            "failed",
            "interrupted"
          ],
          type: "string"
        },
        CommandAction: {
          oneOf: [
            {
              properties: {
                command: {
                  type: "string"
                },
                name: {
                  type: "string"
                },
                path: {
                  $ref: "#/definitions/LegacyAppPathString"
                },
                type: {
                  enum: [
                    "read"
                  ],
                  title: "ReadCommandActionType",
                  type: "string"
                }
              },
              required: [
                "command",
                "name",
                "path",
                "type"
              ],
              title: "ReadCommandAction",
              type: "object"
            },
            {
              properties: {
                command: {
                  type: "string"
                },
                path: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "listFiles"
                  ],
                  title: "ListFilesCommandActionType",
                  type: "string"
                }
              },
              required: [
                "command",
                "type"
              ],
              title: "ListFilesCommandAction",
              type: "object"
            },
            {
              properties: {
                command: {
                  type: "string"
                },
                path: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                query: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "search"
                  ],
                  title: "SearchCommandActionType",
                  type: "string"
                }
              },
              required: [
                "command",
                "type"
              ],
              title: "SearchCommandAction",
              type: "object"
            },
            {
              properties: {
                command: {
                  type: "string"
                },
                type: {
                  enum: [
                    "unknown"
                  ],
                  title: "UnknownCommandActionType",
                  type: "string"
                }
              },
              required: [
                "command",
                "type"
              ],
              title: "UnknownCommandAction",
              type: "object"
            }
          ]
        },
        CommandExecutionSource: {
          enum: [
            "agent",
            "userShell",
            "unifiedExecStartup",
            "unifiedExecInteraction"
          ],
          type: "string"
        },
        CommandExecutionStatus: {
          enum: [
            "inProgress",
            "completed",
            "failed",
            "declined"
          ],
          type: "string"
        },
        DynamicToolCallOutputContentItem: {
          oneOf: [
            {
              properties: {
                text: {
                  type: "string"
                },
                type: {
                  enum: [
                    "inputText"
                  ],
                  title: "InputTextDynamicToolCallOutputContentItemType",
                  type: "string"
                }
              },
              required: [
                "text",
                "type"
              ],
              title: "InputTextDynamicToolCallOutputContentItem",
              type: "object"
            },
            {
              properties: {
                imageUrl: {
                  type: "string"
                },
                type: {
                  enum: [
                    "inputImage"
                  ],
                  title: "InputImageDynamicToolCallOutputContentItemType",
                  type: "string"
                }
              },
              required: [
                "imageUrl",
                "type"
              ],
              title: "InputImageDynamicToolCallOutputContentItem",
              type: "object"
            },
            {
              properties: {
                audioUrl: {
                  type: "string"
                },
                type: {
                  enum: [
                    "inputAudio"
                  ],
                  title: "InputAudioDynamicToolCallOutputContentItemType",
                  type: "string"
                }
              },
              required: [
                "audioUrl",
                "type"
              ],
              title: "InputAudioDynamicToolCallOutputContentItem",
              type: "object"
            }
          ]
        },
        DynamicToolCallStatus: {
          enum: [
            "inProgress",
            "completed",
            "failed"
          ],
          type: "string"
        },
        FileUpdateChange: {
          properties: {
            diff: {
              type: "string"
            },
            kind: {
              $ref: "#/definitions/PatchChangeKind"
            },
            path: {
              type: "string"
            }
          },
          required: [
            "diff",
            "kind",
            "path"
          ],
          type: "object"
        },
        FunctionCallOutputBody: {
          anyOf: [
            {
              type: "string"
            },
            {
              items: {
                $ref: "#/definitions/FunctionCallOutputContentItem"
              },
              type: "array"
            }
          ]
        },
        FunctionCallOutputContentItem: {
          description: "Responses API compatible content items that can be returned by a tool call. This is a subset of ContentItem with the types we support as function call outputs.",
          oneOf: [
            {
              properties: {
                text: {
                  type: "string"
                },
                type: {
                  enum: [
                    "input_text"
                  ],
                  title: "InputTextFunctionCallOutputContentItemType",
                  type: "string"
                }
              },
              required: [
                "text",
                "type"
              ],
              title: "InputTextFunctionCallOutputContentItem",
              type: "object"
            },
            {
              anyOf: [
                {
                  properties: {
                    image_url: {
                      type: "string"
                    }
                  },
                  required: [
                    "image_url"
                  ],
                  title: "ImageUrlFunctionCallOutputContentItem",
                  type: "object"
                },
                {
                  properties: {
                    file_id: {
                      type: "string"
                    }
                  },
                  required: [
                    "file_id"
                  ],
                  title: "FileIdFunctionCallOutputContentItem",
                  type: "object"
                }
              ],
              properties: {
                detail: {
                  anyOf: [
                    {
                      $ref: "#/definitions/ImageDetail"
                    },
                    {
                      type: "null"
                    }
                  ]
                },
                type: {
                  enum: [
                    "input_image"
                  ],
                  title: "InputImageFunctionCallOutputContentItemType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "InputImageFunctionCallOutputContentItem",
              type: "object"
            },
            {
              properties: {
                audio_url: {
                  type: "string"
                },
                type: {
                  enum: [
                    "input_audio"
                  ],
                  title: "InputAudioFunctionCallOutputContentItemType",
                  type: "string"
                }
              },
              required: [
                "audio_url",
                "type"
              ],
              title: "InputAudioFunctionCallOutputContentItem",
              type: "object"
            },
            {
              properties: {
                encrypted_content: {
                  type: "string"
                },
                type: {
                  enum: [
                    "encrypted_content"
                  ],
                  title: "EncryptedContentFunctionCallOutputContentItemType",
                  type: "string"
                }
              },
              required: [
                "encrypted_content",
                "type"
              ],
              title: "EncryptedContentFunctionCallOutputContentItem",
              type: "object"
            }
          ]
        },
        GitInfo: {
          properties: {
            branch: {
              type: [
                "string",
                "null"
              ]
            },
            originUrl: {
              type: [
                "string",
                "null"
              ]
            },
            sha: {
              type: [
                "string",
                "null"
              ]
            }
          },
          type: "object"
        },
        HookPromptFragment: {
          properties: {
            hookRunId: {
              type: "string"
            },
            text: {
              type: "string"
            }
          },
          required: [
            "hookRunId",
            "text"
          ],
          type: "object"
        },
        ImageDetail: {
          enum: [
            "auto",
            "low",
            "high",
            "original"
          ],
          type: "string"
        },
        ImageGenerationFailure: {
          oneOf: [
            {
              properties: {
                limitId: {
                  type: "string"
                },
                resetsAt: {
                  format: "int64",
                  type: [
                    "integer",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "usageLimitExceeded"
                  ],
                  title: "UsageLimitExceededImageGenerationFailureType",
                  type: "string"
                }
              },
              required: [
                "limitId",
                "type"
              ],
              title: "UsageLimitExceededImageGenerationFailure",
              type: "object"
            }
          ]
        },
        LegacyAppPathString: {
          type: "string"
        },
        McpAppDisplayMode: {
          enum: [
            "inline",
            "fullscreen"
          ],
          type: "string"
        },
        McpAppUi: {
          description: "UI resource and display preference for model invocations, captured from the tool descriptor.",
          properties: {
            preferredModelDisplayMode: {
              $ref: "#/definitions/McpAppDisplayMode"
            },
            resourceUri: {
              type: "string"
            }
          },
          required: [
            "preferredModelDisplayMode",
            "resourceUri"
          ],
          type: "object"
        },
        McpToolCallAppContext: {
          properties: {
            actionName: {
              type: [
                "string",
                "null"
              ]
            },
            appName: {
              type: [
                "string",
                "null"
              ]
            },
            connectorId: {
              type: "string"
            },
            linkId: {
              type: [
                "string",
                "null"
              ]
            },
            resourceUri: {
              type: [
                "string",
                "null"
              ]
            }
          },
          required: [
            "connectorId"
          ],
          type: "object"
        },
        McpToolCallError: {
          properties: {
            message: {
              type: "string"
            }
          },
          required: [
            "message"
          ],
          type: "object"
        },
        McpToolCallResult: {
          properties: {
            _meta: true,
            content: {
              items: true,
              type: "array"
            },
            structuredContent: true
          },
          required: [
            "content"
          ],
          type: "object"
        },
        McpToolCallStatus: {
          enum: [
            "inProgress",
            "completed",
            "failed"
          ],
          type: "string"
        },
        MemoryCitation: {
          properties: {
            entries: {
              items: {
                $ref: "#/definitions/MemoryCitationEntry"
              },
              type: "array"
            },
            threadIds: {
              items: {
                type: "string"
              },
              type: "array"
            }
          },
          required: [
            "entries",
            "threadIds"
          ],
          type: "object"
        },
        MemoryCitationEntry: {
          properties: {
            lineEnd: {
              format: "uint32",
              minimum: 0,
              type: "integer"
            },
            lineStart: {
              format: "uint32",
              minimum: 0,
              type: "integer"
            },
            note: {
              type: "string"
            },
            path: {
              type: "string"
            }
          },
          required: [
            "lineEnd",
            "lineStart",
            "note",
            "path"
          ],
          type: "object"
        },
        MessagePhase: {
          description: 'Classifies assistant text as commentary, a partial answer, or a terminal answer.\n\nProviders do not emit this consistently, so callers must treat `None` as "phase unknown" and keep compatibility behavior for legacy models.',
          oneOf: [
            {
              description: "Mid-turn assistant text (for example preamble/progress narration).\n\nAdditional tool calls or assistant output may follow before turn completion.",
              enum: [
                "commentary"
              ],
              type: "string"
            },
            {
              description: "Stable answer text that may be followed by more assistant output or tools.",
              enum: [
                "partial_answer"
              ],
              type: "string"
            },
            {
              description: "The assistant's declared terminal answer text for the current turn.",
              enum: [
                "final_answer"
              ],
              type: "string"
            }
          ]
        },
        MisalignmentErrorDetails: {
          properties: {
            detailedExplanation: {
              description: "A substantive localized explanation is required before offering continuation.",
              type: [
                "string",
                "null"
              ]
            },
            errorType: {
              description: "Open-ended classification; clients must accept categories added by Responses.",
              type: [
                "string",
                "null"
              ]
            },
            reviewTarget: {
              description: "Opaque server-issued block target. Presence alone does not enable target-based continuation.",
              type: [
                "string",
                "null"
              ]
            },
            steer: {
              anyOf: [
                {
                  $ref: "#/definitions/MisalignmentSteer"
                },
                {
                  type: "null"
                }
              ],
              description: "Instruction to submit as the next turn's user input if continuation is confirmed."
            }
          },
          type: "object"
        },
        MisalignmentSteer: {
          properties: {
            message: {
              type: "string"
            }
          },
          required: [
            "message"
          ],
          type: "object"
        },
        MultiAgentMode: {
          description: "Controls the effective multi-agent delegation instructions for a turn. `custom` means the configured mode hint defines the policy instead of a built-in policy.",
          oneOf: [
            {
              enum: [
                "explicitRequestOnly",
                "proactive"
              ],
              type: "string"
            },
            {
              additionalProperties: false,
              properties: {
                custom: {
                  type: "string"
                }
              },
              required: [
                "custom"
              ],
              title: "CustomMultiAgentMode",
              type: "object"
            }
          ]
        },
        NetworkAccess: {
          enum: [
            "restricted",
            "enabled"
          ],
          type: "string"
        },
        NonSteerableTurnKind: {
          enum: [
            "review",
            "compact"
          ],
          type: "string"
        },
        PatchApplyStatus: {
          enum: [
            "inProgress",
            "completed",
            "failed",
            "declined"
          ],
          type: "string"
        },
        PatchChangeKind: {
          oneOf: [
            {
              properties: {
                type: {
                  enum: [
                    "add"
                  ],
                  title: "AddPatchChangeKindType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "AddPatchChangeKind",
              type: "object"
            },
            {
              properties: {
                type: {
                  enum: [
                    "delete"
                  ],
                  title: "DeletePatchChangeKindType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "DeletePatchChangeKind",
              type: "object"
            },
            {
              properties: {
                move_path: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "update"
                  ],
                  title: "UpdatePatchChangeKindType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "UpdatePatchChangeKind",
              type: "object"
            }
          ]
        },
        ReasoningEffort: {
          description: "A non-empty reasoning effort value advertised by the model.",
          minLength: 1,
          type: "string"
        },
        SandboxPolicy: {
          oneOf: [
            {
              properties: {
                type: {
                  enum: [
                    "dangerFullAccess"
                  ],
                  title: "DangerFullAccessSandboxPolicyType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "DangerFullAccessSandboxPolicy",
              type: "object"
            },
            {
              properties: {
                networkAccess: {
                  default: false,
                  type: "boolean"
                },
                type: {
                  enum: [
                    "readOnly"
                  ],
                  title: "ReadOnlySandboxPolicyType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "ReadOnlySandboxPolicy",
              type: "object"
            },
            {
              properties: {
                networkAccess: {
                  allOf: [
                    {
                      $ref: "#/definitions/NetworkAccess"
                    }
                  ],
                  default: "restricted"
                },
                type: {
                  enum: [
                    "externalSandbox"
                  ],
                  title: "ExternalSandboxSandboxPolicyType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "ExternalSandboxSandboxPolicy",
              type: "object"
            },
            {
              properties: {
                excludeSlashTmp: {
                  default: false,
                  type: "boolean"
                },
                excludeTmpdirEnvVar: {
                  default: false,
                  type: "boolean"
                },
                networkAccess: {
                  default: false,
                  type: "boolean"
                },
                type: {
                  enum: [
                    "workspaceWrite"
                  ],
                  title: "WorkspaceWriteSandboxPolicyType",
                  type: "string"
                },
                writableRoots: {
                  default: [],
                  items: {
                    $ref: "#/definitions/AbsolutePathBuf"
                  },
                  type: "array"
                }
              },
              required: [
                "type"
              ],
              title: "WorkspaceWriteSandboxPolicy",
              type: "object"
            }
          ]
        },
        SessionSource: {
          oneOf: [
            {
              enum: [
                "cli",
                "vscode",
                "exec",
                "appServer",
                "unknown"
              ],
              type: "string"
            },
            {
              additionalProperties: false,
              properties: {
                custom: {
                  type: "string"
                }
              },
              required: [
                "custom"
              ],
              title: "CustomSessionSource",
              type: "object"
            },
            {
              additionalProperties: false,
              properties: {
                subAgent: {
                  $ref: "#/definitions/SubAgentSource"
                }
              },
              required: [
                "subAgent"
              ],
              title: "SubAgentSessionSource",
              type: "object"
            }
          ]
        },
        SubAgentActivityKind: {
          enum: [
            "started",
            "interacted",
            "interrupted",
            "completed"
          ],
          type: "string"
        },
        SubAgentSource: {
          oneOf: [
            {
              enum: [
                "review",
                "compact",
                "memory_consolidation"
              ],
              type: "string"
            },
            {
              additionalProperties: false,
              properties: {
                thread_spawn: {
                  properties: {
                    agent_nickname: {
                      default: null,
                      type: [
                        "string",
                        "null"
                      ]
                    },
                    agent_path: {
                      anyOf: [
                        {
                          $ref: "#/definitions/AgentPath"
                        },
                        {
                          type: "null"
                        }
                      ],
                      default: null
                    },
                    agent_role: {
                      default: null,
                      type: [
                        "string",
                        "null"
                      ]
                    },
                    depth: {
                      format: "int32",
                      type: "integer"
                    },
                    parent_thread_id: {
                      $ref: "#/definitions/ThreadId"
                    }
                  },
                  required: [
                    "depth",
                    "parent_thread_id"
                  ],
                  type: "object"
                }
              },
              required: [
                "thread_spawn"
              ],
              title: "ThreadSpawnSubAgentSource",
              type: "object"
            },
            {
              additionalProperties: false,
              properties: {
                other: {
                  type: "string"
                }
              },
              required: [
                "other"
              ],
              title: "OtherSubAgentSource",
              type: "object"
            }
          ]
        },
        TextElement: {
          properties: {
            byteRange: {
              allOf: [
                {
                  $ref: "#/definitions/ByteRange"
                }
              ],
              description: "Byte range in the parent `text` buffer that this element occupies."
            },
            placeholder: {
              description: "Optional human-readable placeholder for the element, displayed in the UI.",
              type: [
                "string",
                "null"
              ]
            }
          },
          required: [
            "byteRange"
          ],
          type: "object"
        },
        Thread: {
          properties: {
            agentNickname: {
              description: "Optional random unique nickname assigned to an AgentControl-spawned sub-agent.",
              type: [
                "string",
                "null"
              ]
            },
            agentRole: {
              description: "Optional role (agent_role) assigned to an AgentControl-spawned sub-agent.",
              type: [
                "string",
                "null"
              ]
            },
            cliVersion: {
              description: "Version of the CLI that created the thread.",
              type: "string"
            },
            createdAt: {
              description: "Unix timestamp (in seconds) when the thread was created.",
              format: "int64",
              type: "integer"
            },
            cwd: {
              allOf: [
                {
                  $ref: "#/definitions/AbsolutePathBuf"
                }
              ],
              description: "Working directory captured for the thread."
            },
            ephemeral: {
              description: "Whether the thread is ephemeral and should not be materialized on disk.",
              type: "boolean"
            },
            forkedFromId: {
              description: "Source thread id when this thread was created by forking another thread.",
              type: [
                "string",
                "null"
              ]
            },
            gitInfo: {
              anyOf: [
                {
                  $ref: "#/definitions/GitInfo"
                },
                {
                  type: "null"
                }
              ],
              description: "Optional Git metadata captured when the thread was created."
            },
            historyMode: {
              allOf: [
                {
                  $ref: "#/definitions/ThreadHistoryMode"
                }
              ],
              default: "legacy",
              description: "Persisted thread history contract selected when this thread was created."
            },
            id: {
              description: "Identifier for this thread. Codex-generated thread IDs are UUIDv7.",
              type: "string"
            },
            model: {
              description: "Current configured model when loaded, otherwise the latest persisted model. Null when unavailable. This is not per-turn execution telemetry.",
              type: [
                "string",
                "null"
              ]
            },
            modelProvider: {
              description: "Model provider used for this thread (for example, 'openai').",
              type: "string"
            },
            name: {
              description: "Optional user-facing thread title.",
              type: [
                "string",
                "null"
              ]
            },
            originator: {
              description: "Originator recorded when the thread was created, independent of its current client or executor. Null when the recorded originator is unavailable.",
              type: [
                "string",
                "null"
              ]
            },
            parentThreadId: {
              description: "The ID of the parent thread. This will only be set if this thread is a subagent.",
              type: [
                "string",
                "null"
              ]
            },
            path: {
              description: "[UNSTABLE] Path to the thread on disk.",
              type: [
                "string",
                "null"
              ]
            },
            preview: {
              description: "Usually the first user message in the thread, if available.",
              type: "string"
            },
            projectId: {
              description: "Canonical project assignment owned by app-server, if any.",
              type: [
                "string",
                "null"
              ]
            },
            reasoningEffort: {
              anyOf: [
                {
                  $ref: "#/definitions/ReasoningEffort"
                },
                {
                  type: "null"
                }
              ],
              description: "Current configured reasoning effort when loaded, otherwise the latest persisted effort. Null when unset or unavailable. This is not per-turn execution telemetry."
            },
            recencyAt: {
              description: "Unix timestamp (in seconds) used for thread recency ordering.",
              format: "int64",
              type: [
                "integer",
                "null"
              ]
            },
            section: {
              anyOf: [
                {
                  $ref: "#/definitions/ThreadSection"
                },
                {
                  type: "null"
                }
              ],
              default: null,
              description: "The independently persisted section selected for this thread, if any."
            },
            sectionEnteredAt: {
              default: null,
              description: "Unix timestamp in seconds when the thread entered its current section.",
              format: "int64",
              type: [
                "integer",
                "null"
              ]
            },
            sessionId: {
              description: "Session id shared by threads that belong to the same session tree.",
              type: "string"
            },
            source: {
              allOf: [
                {
                  $ref: "#/definitions/SessionSource"
                }
              ],
              description: "Origin of the thread (CLI, VSCode, codex exec, codex app-server, etc.)."
            },
            status: {
              allOf: [
                {
                  $ref: "#/definitions/ThreadStatus"
                }
              ],
              description: "Current runtime status for the thread."
            },
            threadSource: {
              anyOf: [
                {
                  $ref: "#/definitions/ThreadSource"
                },
                {
                  type: "null"
                }
              ],
              description: "Optional analytics source classification for this thread."
            },
            turns: {
              description: "Only populated on `thread/resume`, `thread/fork`, and `thread/read` (when `includeTurns` is true) responses. For all other responses and notifications returning a Thread, the turns field will be an empty list.",
              items: {
                $ref: "#/definitions/Turn"
              },
              type: "array"
            },
            updatedAt: {
              description: "Unix timestamp (in seconds) when the thread was last updated.",
              format: "int64",
              type: "integer"
            }
          },
          required: [
            "cliVersion",
            "createdAt",
            "cwd",
            "ephemeral",
            "id",
            "modelProvider",
            "preview",
            "projectId",
            "sessionId",
            "source",
            "status",
            "turns",
            "updatedAt"
          ],
          type: "object"
        },
        ThreadActiveFlag: {
          enum: [
            "waitingOnApproval",
            "waitingOnUserInput"
          ],
          type: "string"
        },
        ThreadEnvironment: {
          description: "An environment selected by a loaded thread, independent of connection status.",
          properties: {
            cwd: {
              $ref: "#/definitions/LegacyAppPathString"
            },
            environmentId: {
              type: "string"
            },
            runtimeWorkspaceRoots: {
              items: {
                $ref: "#/definitions/LegacyAppPathString"
              },
              type: "array"
            }
          },
          required: [
            "cwd",
            "environmentId",
            "runtimeWorkspaceRoots"
          ],
          type: "object"
        },
        ThreadExtra: {
          description: "Extra app-server data for a thread.",
          type: "object"
        },
        ThreadHistoryMode: {
          enum: [
            "legacy",
            "paginated"
          ],
          type: "string"
        },
        ThreadId: {
          type: "string"
        },
        ThreadItem: {
          oneOf: [
            {
              properties: {
                clientId: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                content: {
                  items: {
                    $ref: "#/definitions/UserInput"
                  },
                  type: "array"
                },
                id: {
                  type: "string"
                },
                type: {
                  enum: [
                    "userMessage"
                  ],
                  title: "UserMessageThreadItemType",
                  type: "string"
                }
              },
              required: [
                "content",
                "id",
                "type"
              ],
              title: "UserMessageThreadItem",
              type: "object"
            },
            {
              properties: {
                fragments: {
                  items: {
                    $ref: "#/definitions/HookPromptFragment"
                  },
                  type: "array"
                },
                id: {
                  type: "string"
                },
                type: {
                  enum: [
                    "hookPrompt"
                  ],
                  title: "HookPromptThreadItemType",
                  type: "string"
                }
              },
              required: [
                "fragments",
                "id",
                "type"
              ],
              title: "HookPromptThreadItem",
              type: "object"
            },
            {
              properties: {
                delivery: {
                  anyOf: [
                    {
                      $ref: "#/definitions/AgentMessageDelivery"
                    },
                    {
                      type: "null"
                    }
                  ],
                  default: null
                },
                id: {
                  type: "string"
                },
                memoryCitation: {
                  anyOf: [
                    {
                      $ref: "#/definitions/MemoryCitation"
                    },
                    {
                      type: "null"
                    }
                  ],
                  default: null
                },
                phase: {
                  anyOf: [
                    {
                      $ref: "#/definitions/MessagePhase"
                    },
                    {
                      type: "null"
                    }
                  ],
                  default: null
                },
                questions: {
                  default: null,
                  items: {
                    $ref: "#/definitions/AsyncUserInputQuestion"
                  },
                  type: [
                    "array",
                    "null"
                  ]
                },
                text: {
                  type: "string"
                },
                type: {
                  enum: [
                    "agentMessage"
                  ],
                  title: "AgentMessageThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "text",
                "type"
              ],
              title: "AgentMessageThreadItem",
              type: "object"
            },
            {
              properties: {
                id: {
                  type: "string"
                },
                name: {
                  type: "string"
                },
                namespace: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                output: {
                  $ref: "#/definitions/FunctionCallOutputBody"
                },
                type: {
                  enum: [
                    "functionCallOutput"
                  ],
                  title: "FunctionCallOutputThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "name",
                "output",
                "type"
              ],
              title: "FunctionCallOutputThreadItem",
              type: "object"
            },
            {
              description: "EXPERIMENTAL - proposed plan item content. The completed plan item is authoritative and may not match the concatenation of `PlanDelta` text.",
              properties: {
                id: {
                  type: "string"
                },
                text: {
                  type: "string"
                },
                type: {
                  enum: [
                    "plan"
                  ],
                  title: "PlanThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "text",
                "type"
              ],
              title: "PlanThreadItem",
              type: "object"
            },
            {
              properties: {
                content: {
                  default: [],
                  items: {
                    type: "string"
                  },
                  type: "array"
                },
                id: {
                  type: "string"
                },
                summary: {
                  default: [],
                  items: {
                    type: "string"
                  },
                  type: "array"
                },
                type: {
                  enum: [
                    "reasoning"
                  ],
                  title: "ReasoningThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "type"
              ],
              title: "ReasoningThreadItem",
              type: "object"
            },
            {
              properties: {
                aggregatedOutput: {
                  description: "The command's output, aggregated from stdout and stderr.",
                  type: [
                    "string",
                    "null"
                  ]
                },
                command: {
                  description: "The command to be executed.",
                  type: "string"
                },
                commandActions: {
                  description: "A best-effort parsing of the command to understand the action(s) it will perform. This returns a list of CommandAction objects because a single shell command may be composed of many commands piped together.",
                  items: {
                    $ref: "#/definitions/CommandAction"
                  },
                  type: "array"
                },
                cwd: {
                  allOf: [
                    {
                      $ref: "#/definitions/LegacyAppPathString"
                    }
                  ],
                  description: "The command's working directory."
                },
                durationMs: {
                  description: "The duration of the command execution in milliseconds.",
                  format: "int64",
                  type: [
                    "integer",
                    "null"
                  ]
                },
                exitCode: {
                  description: "The command's exit code.",
                  format: "int32",
                  type: [
                    "integer",
                    "null"
                  ]
                },
                id: {
                  type: "string"
                },
                pluginId: {
                  default: null,
                  description: "Trusted first-party plugin id when this command resolves to one plugin script.",
                  type: [
                    "string",
                    "null"
                  ]
                },
                processId: {
                  description: "Identifier for the underlying PTY process (when available).",
                  type: [
                    "string",
                    "null"
                  ]
                },
                scriptPath: {
                  default: null,
                  description: "Safe plugin-relative path when this command resolves to one plugin script.",
                  type: [
                    "string",
                    "null"
                  ]
                },
                source: {
                  allOf: [
                    {
                      $ref: "#/definitions/CommandExecutionSource"
                    }
                  ],
                  default: "agent"
                },
                status: {
                  $ref: "#/definitions/CommandExecutionStatus"
                },
                type: {
                  enum: [
                    "commandExecution"
                  ],
                  title: "CommandExecutionThreadItemType",
                  type: "string"
                }
              },
              required: [
                "command",
                "commandActions",
                "cwd",
                "id",
                "status",
                "type"
              ],
              title: "CommandExecutionThreadItem",
              type: "object"
            },
            {
              properties: {
                changes: {
                  items: {
                    $ref: "#/definitions/FileUpdateChange"
                  },
                  type: "array"
                },
                id: {
                  type: "string"
                },
                status: {
                  $ref: "#/definitions/PatchApplyStatus"
                },
                type: {
                  enum: [
                    "fileChange"
                  ],
                  title: "FileChangeThreadItemType",
                  type: "string"
                }
              },
              required: [
                "changes",
                "id",
                "status",
                "type"
              ],
              title: "FileChangeThreadItem",
              type: "object"
            },
            {
              properties: {
                appContext: {
                  anyOf: [
                    {
                      $ref: "#/definitions/McpToolCallAppContext"
                    },
                    {
                      type: "null"
                    }
                  ]
                },
                arguments: true,
                durationMs: {
                  description: "The duration of the MCP tool call in milliseconds.",
                  format: "int64",
                  type: [
                    "integer",
                    "null"
                  ]
                },
                error: {
                  anyOf: [
                    {
                      $ref: "#/definitions/McpToolCallError"
                    },
                    {
                      type: "null"
                    }
                  ]
                },
                id: {
                  type: "string"
                },
                mcpAppResourceUri: {
                  description: "Legacy compatibility field; prefer `mcpAppUi.resourceUri` when available.",
                  type: [
                    "string",
                    "null"
                  ]
                },
                mcpAppUi: {
                  anyOf: [
                    {
                      $ref: "#/definitions/McpAppUi"
                    },
                    {
                      type: "null"
                    }
                  ],
                  description: "Presentation captured from the invoked descriptor; absent in older history."
                },
                pluginId: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                readOnlyHint: {
                  type: [
                    "boolean",
                    "null"
                  ]
                },
                result: {
                  anyOf: [
                    {
                      $ref: "#/definitions/McpToolCallResult"
                    },
                    {
                      type: "null"
                    }
                  ]
                },
                server: {
                  type: "string"
                },
                status: {
                  $ref: "#/definitions/McpToolCallStatus"
                },
                tool: {
                  type: "string"
                },
                type: {
                  enum: [
                    "mcpToolCall"
                  ],
                  title: "McpToolCallThreadItemType",
                  type: "string"
                }
              },
              required: [
                "arguments",
                "id",
                "server",
                "status",
                "tool",
                "type"
              ],
              title: "McpToolCallThreadItem",
              type: "object"
            },
            {
              properties: {
                arguments: true,
                contentItems: {
                  items: {
                    $ref: "#/definitions/DynamicToolCallOutputContentItem"
                  },
                  type: [
                    "array",
                    "null"
                  ]
                },
                durationMs: {
                  description: "The duration of the dynamic tool call in milliseconds.",
                  format: "int64",
                  type: [
                    "integer",
                    "null"
                  ]
                },
                id: {
                  type: "string"
                },
                namespace: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                status: {
                  $ref: "#/definitions/DynamicToolCallStatus"
                },
                success: {
                  type: [
                    "boolean",
                    "null"
                  ]
                },
                tool: {
                  type: "string"
                },
                type: {
                  enum: [
                    "dynamicToolCall"
                  ],
                  title: "DynamicToolCallThreadItemType",
                  type: "string"
                }
              },
              required: [
                "arguments",
                "id",
                "status",
                "tool",
                "type"
              ],
              title: "DynamicToolCallThreadItem",
              type: "object"
            },
            {
              properties: {
                agentsStates: {
                  additionalProperties: {
                    $ref: "#/definitions/CollabAgentState"
                  },
                  description: "Last known status of the target agents, when available.",
                  type: "object"
                },
                id: {
                  description: "Unique identifier for this collab tool call.",
                  type: "string"
                },
                model: {
                  description: "Model requested for the spawned agent, when applicable.",
                  type: [
                    "string",
                    "null"
                  ]
                },
                prompt: {
                  description: "Prompt text sent as part of the collab tool call, when available.",
                  type: [
                    "string",
                    "null"
                  ]
                },
                reasoningEffort: {
                  anyOf: [
                    {
                      $ref: "#/definitions/ReasoningEffort"
                    },
                    {
                      type: "null"
                    }
                  ],
                  description: "Reasoning effort requested for the spawned agent, when applicable."
                },
                receiverThreadIds: {
                  description: "Thread ID of the receiving agent, when applicable. In case of spawn operation, this corresponds to the newly spawned agent.",
                  items: {
                    type: "string"
                  },
                  type: "array"
                },
                senderThreadId: {
                  description: "Thread ID of the agent issuing the collab request.",
                  type: "string"
                },
                status: {
                  allOf: [
                    {
                      $ref: "#/definitions/CollabAgentToolCallStatus"
                    }
                  ],
                  description: "Current status of the collab tool call."
                },
                tool: {
                  allOf: [
                    {
                      $ref: "#/definitions/CollabAgentTool"
                    }
                  ],
                  description: "Name of the collab tool that was invoked."
                },
                type: {
                  enum: [
                    "collabAgentToolCall"
                  ],
                  title: "CollabAgentToolCallThreadItemType",
                  type: "string"
                }
              },
              required: [
                "agentsStates",
                "id",
                "receiverThreadIds",
                "senderThreadId",
                "status",
                "tool",
                "type"
              ],
              title: "CollabAgentToolCallThreadItem",
              type: "object"
            },
            {
              properties: {
                agentPath: {
                  type: "string"
                },
                agentThreadId: {
                  type: "string"
                },
                id: {
                  type: "string"
                },
                kind: {
                  $ref: "#/definitions/SubAgentActivityKind"
                },
                model: {
                  description: "Resolved model at sub-agent creation; absent from older records and other activities.",
                  type: [
                    "string",
                    "null"
                  ]
                },
                reasoningEffort: {
                  anyOf: [
                    {
                      $ref: "#/definitions/ReasoningEffort"
                    },
                    {
                      type: "null"
                    }
                  ],
                  description: "Resolved reasoning effort at sub-agent creation, when known."
                },
                type: {
                  enum: [
                    "subAgentActivity"
                  ],
                  title: "SubAgentActivityThreadItemType",
                  type: "string"
                }
              },
              required: [
                "agentPath",
                "agentThreadId",
                "id",
                "kind",
                "type"
              ],
              title: "SubAgentActivityThreadItem",
              type: "object"
            },
            {
              properties: {
                action: {
                  anyOf: [
                    {
                      $ref: "#/definitions/WebSearchAction"
                    },
                    {
                      type: "null"
                    }
                  ]
                },
                id: {
                  type: "string"
                },
                query: {
                  type: "string"
                },
                results: {
                  default: null,
                  description: "Structured search results returned out-of-band by standalone web search.\n\nThese stay as opaque JSON at the extension/app-server boundary so new result fields and result types can pass through without a Codex release.",
                  items: true,
                  type: [
                    "array",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "webSearch"
                  ],
                  title: "WebSearchThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "query",
                "type"
              ],
              title: "WebSearchThreadItem",
              type: "object"
            },
            {
              properties: {
                id: {
                  type: "string"
                },
                path: {
                  $ref: "#/definitions/LegacyAppPathString"
                },
                type: {
                  enum: [
                    "imageView"
                  ],
                  title: "ImageViewThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "path",
                "type"
              ],
              title: "ImageViewThreadItem",
              type: "object"
            },
            {
              description: "Display item emitted by the interruptible `clock.sleep` tool.",
              properties: {
                durationMs: {
                  format: "uint64",
                  minimum: 0,
                  type: "integer"
                },
                id: {
                  type: "string"
                },
                type: {
                  enum: [
                    "sleep"
                  ],
                  title: "SleepThreadItemType",
                  type: "string"
                }
              },
              required: [
                "durationMs",
                "id",
                "type"
              ],
              title: "SleepThreadItem",
              type: "object"
            },
            {
              properties: {
                failure: {
                  anyOf: [
                    {
                      $ref: "#/definitions/ImageGenerationFailure"
                    },
                    {
                      type: "null"
                    }
                  ],
                  default: null
                },
                id: {
                  type: "string"
                },
                result: {
                  type: "string"
                },
                revisedPrompt: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                savedPath: {
                  anyOf: [
                    {
                      $ref: "#/definitions/AbsolutePathBuf"
                    },
                    {
                      type: "null"
                    }
                  ]
                },
                status: {
                  type: "string"
                },
                transparentBackground: {
                  default: null,
                  type: [
                    "boolean",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "imageGeneration"
                  ],
                  title: "ImageGenerationThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "result",
                "status",
                "type"
              ],
              title: "ImageGenerationThreadItem",
              type: "object"
            },
            {
              properties: {
                id: {
                  type: "string"
                },
                review: {
                  type: "string"
                },
                type: {
                  enum: [
                    "enteredReviewMode"
                  ],
                  title: "EnteredReviewModeThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "review",
                "type"
              ],
              title: "EnteredReviewModeThreadItem",
              type: "object"
            },
            {
              properties: {
                id: {
                  type: "string"
                },
                review: {
                  type: "string"
                },
                type: {
                  enum: [
                    "exitedReviewMode"
                  ],
                  title: "ExitedReviewModeThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "review",
                "type"
              ],
              title: "ExitedReviewModeThreadItem",
              type: "object"
            },
            {
              properties: {
                id: {
                  type: "string"
                },
                type: {
                  enum: [
                    "contextCompaction"
                  ],
                  title: "ContextCompactionThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "type"
              ],
              title: "ContextCompactionThreadItem",
              type: "object"
            }
          ]
        },
        ThreadSection: {
          description: "An independently persisted, user-visible thread section.",
          properties: {
            appearance: {
              anyOf: [
                {
                  $ref: "#/definitions/ThreadSectionAppearance"
                },
                {
                  type: "null"
                }
              ],
              default: null,
              description: "Optional appearance synchronized across clients."
            },
            id: {
              description: "Opaque UUIDv7 identity that remains stable when the section is renamed.",
              type: "string"
            },
            name: {
              description: "The current user-visible section name.",
              type: "string"
            }
          },
          required: [
            "id",
            "name"
          ],
          type: "object"
        },
        ThreadSectionAppearance: {
          description: "Extensible visual presentation for a custom thread section.",
          properties: {
            color: {
              type: [
                "string",
                "null"
              ]
            },
            icon: {
              type: [
                "string",
                "null"
              ]
            }
          },
          type: "object"
        },
        ThreadSource: {
          type: "string"
        },
        ThreadStatus: {
          oneOf: [
            {
              properties: {
                type: {
                  enum: [
                    "notLoaded"
                  ],
                  title: "NotLoadedThreadStatusType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "NotLoadedThreadStatus",
              type: "object"
            },
            {
              properties: {
                type: {
                  enum: [
                    "idle"
                  ],
                  title: "IdleThreadStatusType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "IdleThreadStatus",
              type: "object"
            },
            {
              properties: {
                type: {
                  enum: [
                    "systemError"
                  ],
                  title: "SystemErrorThreadStatusType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "SystemErrorThreadStatus",
              type: "object"
            },
            {
              properties: {
                activeFlags: {
                  items: {
                    $ref: "#/definitions/ThreadActiveFlag"
                  },
                  type: "array"
                },
                type: {
                  enum: [
                    "active"
                  ],
                  title: "ActiveThreadStatusType",
                  type: "string"
                }
              },
              required: [
                "activeFlags",
                "type"
              ],
              title: "ActiveThreadStatus",
              type: "object"
            }
          ]
        },
        Turn: {
          properties: {
            completedAt: {
              description: "Unix timestamp (in seconds) when the turn completed.",
              format: "int64",
              type: [
                "integer",
                "null"
              ]
            },
            durationMs: {
              description: "Duration between turn start and completion in milliseconds, if known.",
              format: "int64",
              type: [
                "integer",
                "null"
              ]
            },
            error: {
              anyOf: [
                {
                  $ref: "#/definitions/TurnError"
                },
                {
                  type: "null"
                }
              ],
              description: "Error associated with a failed or interrupted turn."
            },
            id: {
              description: "Identifier for this turn. Codex-generated turn IDs are UUIDv7.",
              type: "string"
            },
            items: {
              description: "Thread items currently included in this turn payload.",
              items: {
                $ref: "#/definitions/ThreadItem"
              },
              type: "array"
            },
            itemsView: {
              allOf: [
                {
                  $ref: "#/definitions/TurnItemsView"
                }
              ],
              default: "full",
              description: "Describes how much of `items` has been loaded for this turn."
            },
            rootTurnId: {
              description: "ID of the first turn in the chain of work that led to this turn. Pass this as `rootTurnId` when starting work on behalf of this turn. May be null in older history or a `review/start` response.",
              type: [
                "string",
                "null"
              ]
            },
            startedAt: {
              description: "Unix timestamp (in seconds) when the turn started.",
              format: "int64",
              type: [
                "integer",
                "null"
              ]
            },
            status: {
              $ref: "#/definitions/TurnStatus"
            }
          },
          required: [
            "id",
            "items",
            "status"
          ],
          type: "object"
        },
        TurnError: {
          properties: {
            additionalDetails: {
              default: null,
              type: [
                "string",
                "null"
              ]
            },
            codexErrorInfo: {
              anyOf: [
                {
                  $ref: "#/definitions/CodexErrorInfo"
                },
                {
                  type: "null"
                }
              ]
            },
            message: {
              type: "string"
            },
            misalignment: {
              anyOf: [
                {
                  $ref: "#/definitions/MisalignmentErrorDetails"
                },
                {
                  type: "null"
                }
              ],
              default: null,
              description: "Optional public explanation and continuation instruction for a misalignment block."
            }
          },
          required: [
            "message"
          ],
          type: "object"
        },
        TurnItemsView: {
          oneOf: [
            {
              description: "`items` was not loaded for this turn. The field is intentionally empty.",
              enum: [
                "notLoaded"
              ],
              type: "string"
            },
            {
              description: "`items` contains only a display summary for this turn.",
              enum: [
                "summary"
              ],
              type: "string"
            },
            {
              description: "`items` contains every ThreadItem available from persisted app-server history for this turn.",
              enum: [
                "full"
              ],
              type: "string"
            }
          ]
        },
        TurnStatus: {
          enum: [
            "completed",
            "interrupted",
            "failed",
            "inProgress"
          ],
          type: "string"
        },
        UserInput: {
          oneOf: [
            {
              properties: {
                text: {
                  type: "string"
                },
                text_elements: {
                  default: [],
                  description: "UI-defined spans within `text` used to render or persist special elements.",
                  items: {
                    $ref: "#/definitions/TextElement"
                  },
                  type: "array"
                },
                type: {
                  enum: [
                    "text"
                  ],
                  title: "TextUserInputType",
                  type: "string"
                }
              },
              required: [
                "text",
                "type"
              ],
              title: "TextUserInput",
              type: "object"
            },
            {
              anyOf: [
                {
                  properties: {
                    url: {
                      type: "string"
                    }
                  },
                  required: [
                    "url"
                  ],
                  title: "UrlUserInput",
                  type: "object"
                },
                {
                  properties: {
                    fileId: {
                      type: "string"
                    }
                  },
                  required: [
                    "fileId"
                  ],
                  title: "FileIdUserInput",
                  type: "object"
                }
              ],
              properties: {
                detail: {
                  anyOf: [
                    {
                      $ref: "#/definitions/ImageDetail"
                    },
                    {
                      type: "null"
                    }
                  ],
                  default: null
                },
                type: {
                  enum: [
                    "image"
                  ],
                  title: "ImageUserInputType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "ImageUserInput",
              type: "object"
            },
            {
              properties: {
                detail: {
                  anyOf: [
                    {
                      $ref: "#/definitions/ImageDetail"
                    },
                    {
                      type: "null"
                    }
                  ],
                  default: null
                },
                path: {
                  type: "string"
                },
                type: {
                  enum: [
                    "localImage"
                  ],
                  title: "LocalImageUserInputType",
                  type: "string"
                }
              },
              required: [
                "path",
                "type"
              ],
              title: "LocalImageUserInput",
              type: "object"
            },
            {
              properties: {
                type: {
                  enum: [
                    "audio"
                  ],
                  title: "AudioUserInputType",
                  type: "string"
                },
                url: {
                  type: "string"
                }
              },
              required: [
                "type",
                "url"
              ],
              title: "AudioUserInput",
              type: "object"
            },
            {
              properties: {
                path: {
                  type: "string"
                },
                type: {
                  enum: [
                    "localAudio"
                  ],
                  title: "LocalAudioUserInputType",
                  type: "string"
                }
              },
              required: [
                "path",
                "type"
              ],
              title: "LocalAudioUserInput",
              type: "object"
            },
            {
              properties: {
                name: {
                  type: "string"
                },
                path: {
                  type: "string"
                },
                type: {
                  enum: [
                    "skill"
                  ],
                  title: "SkillUserInputType",
                  type: "string"
                }
              },
              required: [
                "name",
                "path",
                "type"
              ],
              title: "SkillUserInput",
              type: "object"
            },
            {
              properties: {
                name: {
                  type: "string"
                },
                path: {
                  type: "string"
                },
                type: {
                  enum: [
                    "mention"
                  ],
                  title: "MentionUserInputType",
                  type: "string"
                }
              },
              required: [
                "name",
                "path",
                "type"
              ],
              title: "MentionUserInput",
              type: "object"
            }
          ]
        },
        WebSearchAction: {
          oneOf: [
            {
              properties: {
                queries: {
                  items: {
                    type: "string"
                  },
                  type: [
                    "array",
                    "null"
                  ]
                },
                query: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "search"
                  ],
                  title: "SearchWebSearchActionType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "SearchWebSearchAction",
              type: "object"
            },
            {
              properties: {
                type: {
                  enum: [
                    "openPage"
                  ],
                  title: "OpenPageWebSearchActionType",
                  type: "string"
                },
                url: {
                  type: [
                    "string",
                    "null"
                  ]
                }
              },
              required: [
                "type"
              ],
              title: "OpenPageWebSearchAction",
              type: "object"
            },
            {
              properties: {
                pattern: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "findInPage"
                  ],
                  title: "FindInPageWebSearchActionType",
                  type: "string"
                },
                url: {
                  type: [
                    "string",
                    "null"
                  ]
                }
              },
              required: [
                "type"
              ],
              title: "FindInPageWebSearchAction",
              type: "object"
            },
            {
              properties: {
                type: {
                  enum: [
                    "other"
                  ],
                  title: "OtherWebSearchActionType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "OtherWebSearchAction",
              type: "object"
            }
          ]
        }
      },
      properties: {
        approvalPolicy: {
          $ref: "#/definitions/AskForApproval"
        },
        approvalsReviewer: {
          allOf: [
            {
              $ref: "#/definitions/ApprovalsReviewer"
            }
          ],
          description: "Reviewer currently used for approval requests on this thread."
        },
        cwd: {
          $ref: "#/definitions/AbsolutePathBuf"
        },
        disabledPluginIds: {
          default: [],
          description: "Saved list of disabled plugin IDs. Does not yet filter plugin capabilities.",
          items: {
            type: "string"
          },
          type: "array"
        },
        instructionSources: {
          default: [],
          description: "Environment-native paths to instruction source files currently loaded for this thread.",
          items: {
            $ref: "#/definitions/LegacyAppPathString"
          },
          type: "array"
        },
        model: {
          type: "string"
        },
        modelProvider: {
          type: "string"
        },
        reasoningEffort: {
          anyOf: [
            {
              $ref: "#/definitions/ReasoningEffort"
            },
            {
              type: "null"
            }
          ]
        },
        sandbox: {
          allOf: [
            {
              $ref: "#/definitions/SandboxPolicy"
            }
          ],
          description: "Legacy sandbox policy retained for compatibility. Experimental clients should prefer `activePermissionProfile` for profile provenance."
        },
        serviceTier: {
          type: [
            "string",
            "null"
          ]
        },
        thread: {
          $ref: "#/definitions/Thread"
        }
      },
      required: [
        "approvalPolicy",
        "approvalsReviewer",
        "cwd",
        "model",
        "modelProvider",
        "sandbox",
        "thread"
      ],
      title: "ThreadStartResponse",
      type: "object"
    };
  }
});

// src/codex-protocol/schema/v2/ThreadResumeResponse.json
var ThreadResumeResponse_default;
var init_ThreadResumeResponse = __esm({
  "src/codex-protocol/schema/v2/ThreadResumeResponse.json"() {
    ThreadResumeResponse_default = {
      $schema: "http://json-schema.org/draft-07/schema#",
      definitions: {
        AbsolutePathBuf: {
          description: "A path that is guaranteed to be absolute and normalized (though it is not guaranteed to be canonicalized or exist on the filesystem).\n\nIMPORTANT: When deserializing an `AbsolutePathBuf`, a base path must be set using [AbsolutePathBufGuard::new]. If no base path is set, the deserialization will fail unless the path being deserialized is already absolute.",
          type: "string"
        },
        ActivePermissionProfile: {
          properties: {
            extends: {
              default: null,
              description: "Parent profile identifier from the selected permissions profile's `extends` setting, when present.",
              type: [
                "string",
                "null"
              ]
            },
            id: {
              description: "Identifier from `default_permissions` or the implicit built-in default, such as `:workspace` or a user-defined `[permissions.<id>]` profile.",
              type: "string"
            }
          },
          required: [
            "id"
          ],
          type: "object"
        },
        AgentMessageDelivery: {
          enum: [
            "async"
          ],
          type: "string"
        },
        AgentPath: {
          type: "string"
        },
        ApprovalsReviewer: {
          description: "Configures who approval requests are routed to for review. Examples include sandbox escapes, blocked network access, MCP approval prompts, and ARC escalations. Defaults to `user`. `auto_review` uses a carefully prompted subagent to gather relevant context and apply a risk-based decision framework before approving or denying the request. The legacy value `guardian_subagent` is accepted for compatibility.",
          enum: [
            "user",
            "auto_review",
            "guardian_subagent"
          ],
          type: "string"
        },
        AskForApproval: {
          oneOf: [
            {
              enum: [
                "untrusted",
                "on-request",
                "never"
              ],
              type: "string"
            },
            {
              additionalProperties: false,
              properties: {
                granular: {
                  properties: {
                    mcp_elicitations: {
                      type: "boolean"
                    },
                    request_permissions: {
                      default: false,
                      type: "boolean"
                    },
                    rules: {
                      type: "boolean"
                    },
                    sandbox_approval: {
                      type: "boolean"
                    },
                    skill_approval: {
                      default: false,
                      type: "boolean"
                    }
                  },
                  required: [
                    "mcp_elicitations",
                    "rules",
                    "sandbox_approval"
                  ],
                  type: "object"
                }
              },
              required: [
                "granular"
              ],
              title: "GranularAskForApproval",
              type: "object"
            }
          ]
        },
        AsyncUserInputQuestion: {
          additionalProperties: false,
          properties: {
            options: {
              items: {
                type: "string"
              },
              type: [
                "array",
                "null"
              ]
            },
            title: {
              type: "string"
            }
          },
          required: [
            "title"
          ],
          type: "object"
        },
        ByteRange: {
          properties: {
            end: {
              format: "uint",
              minimum: 0,
              type: "integer"
            },
            start: {
              format: "uint",
              minimum: 0,
              type: "integer"
            }
          },
          required: [
            "end",
            "start"
          ],
          type: "object"
        },
        CodexErrorInfo: {
          anyOf: [
            {
              enum: [
                "contextWindowExceeded",
                "sessionBudgetExceeded",
                "usageLimitExceeded",
                "rateLimitExceeded",
                "flexUnavailable",
                "serverOverloaded",
                "cyberPolicy",
                "misalignmentPolicyViolation",
                "tooManyDenials",
                "internalServerError",
                "unauthorized",
                "badRequest",
                "threadRollbackFailed",
                "sandboxError",
                "other"
              ],
              type: "string"
            },
            {
              additionalProperties: false,
              properties: {
                httpConnectionFailed: {
                  properties: {
                    httpStatusCode: {
                      format: "uint16",
                      minimum: 0,
                      type: [
                        "integer",
                        "null"
                      ]
                    }
                  },
                  type: "object"
                }
              },
              required: [
                "httpConnectionFailed"
              ],
              title: "HttpConnectionFailedCodexErrorInfo",
              type: "object"
            },
            {
              additionalProperties: false,
              description: "Failed to connect to the response SSE stream.",
              properties: {
                responseStreamConnectionFailed: {
                  properties: {
                    httpStatusCode: {
                      format: "uint16",
                      minimum: 0,
                      type: [
                        "integer",
                        "null"
                      ]
                    }
                  },
                  type: "object"
                }
              },
              required: [
                "responseStreamConnectionFailed"
              ],
              title: "ResponseStreamConnectionFailedCodexErrorInfo",
              type: "object"
            },
            {
              additionalProperties: false,
              description: "The response SSE stream disconnected in the middle of a turn before completion.",
              properties: {
                responseStreamDisconnected: {
                  properties: {
                    httpStatusCode: {
                      format: "uint16",
                      minimum: 0,
                      type: [
                        "integer",
                        "null"
                      ]
                    }
                  },
                  type: "object"
                }
              },
              required: [
                "responseStreamDisconnected"
              ],
              title: "ResponseStreamDisconnectedCodexErrorInfo",
              type: "object"
            },
            {
              additionalProperties: false,
              description: "Reached the retry limit for responses.",
              properties: {
                responseTooManyFailedAttempts: {
                  properties: {
                    httpStatusCode: {
                      format: "uint16",
                      minimum: 0,
                      type: [
                        "integer",
                        "null"
                      ]
                    }
                  },
                  type: "object"
                }
              },
              required: [
                "responseTooManyFailedAttempts"
              ],
              title: "ResponseTooManyFailedAttemptsCodexErrorInfo",
              type: "object"
            },
            {
              additionalProperties: false,
              description: "Returned when `turn/start` or `turn/steer` is submitted while the current active turn cannot accept same-turn steering, for example `/review` or manual `/compact`.",
              properties: {
                activeTurnNotSteerable: {
                  properties: {
                    turnKind: {
                      $ref: "#/definitions/NonSteerableTurnKind"
                    }
                  },
                  required: [
                    "turnKind"
                  ],
                  type: "object"
                }
              },
              required: [
                "activeTurnNotSteerable"
              ],
              title: "ActiveTurnNotSteerableCodexErrorInfo",
              type: "object"
            },
            {
              type: [
                "string",
                "object"
              ]
            }
          ],
          description: "This translation layer make sure that we expose codex error code in camel case.\n\nWhen an upstream HTTP status is available (for example, from the Responses API or a provider), it is forwarded in `httpStatusCode` on the relevant `codexErrorInfo` variant."
        },
        CollabAgentState: {
          properties: {
            message: {
              type: [
                "string",
                "null"
              ]
            },
            status: {
              $ref: "#/definitions/CollabAgentStatus"
            }
          },
          required: [
            "status"
          ],
          type: "object"
        },
        CollabAgentStatus: {
          enum: [
            "pendingInit",
            "running",
            "interrupted",
            "completed",
            "errored",
            "shutdown",
            "notFound"
          ],
          type: "string"
        },
        CollabAgentTool: {
          enum: [
            "spawnAgent",
            "sendInput",
            "resumeAgent",
            "wait",
            "closeAgent",
            "sendMessage",
            "followupTask",
            "interruptAgent",
            "listAgents"
          ],
          type: "string"
        },
        CollabAgentToolCallStatus: {
          enum: [
            "inProgress",
            "completed",
            "failed",
            "interrupted"
          ],
          type: "string"
        },
        CollaborationMode: {
          description: "Collaboration mode for a Codex session.",
          properties: {
            mode: {
              $ref: "#/definitions/ModeKind"
            },
            settings: {
              $ref: "#/definitions/Settings"
            }
          },
          required: [
            "mode",
            "settings"
          ],
          type: "object"
        },
        CommandAction: {
          oneOf: [
            {
              properties: {
                command: {
                  type: "string"
                },
                name: {
                  type: "string"
                },
                path: {
                  $ref: "#/definitions/LegacyAppPathString"
                },
                type: {
                  enum: [
                    "read"
                  ],
                  title: "ReadCommandActionType",
                  type: "string"
                }
              },
              required: [
                "command",
                "name",
                "path",
                "type"
              ],
              title: "ReadCommandAction",
              type: "object"
            },
            {
              properties: {
                command: {
                  type: "string"
                },
                path: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "listFiles"
                  ],
                  title: "ListFilesCommandActionType",
                  type: "string"
                }
              },
              required: [
                "command",
                "type"
              ],
              title: "ListFilesCommandAction",
              type: "object"
            },
            {
              properties: {
                command: {
                  type: "string"
                },
                path: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                query: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "search"
                  ],
                  title: "SearchCommandActionType",
                  type: "string"
                }
              },
              required: [
                "command",
                "type"
              ],
              title: "SearchCommandAction",
              type: "object"
            },
            {
              properties: {
                command: {
                  type: "string"
                },
                type: {
                  enum: [
                    "unknown"
                  ],
                  title: "UnknownCommandActionType",
                  type: "string"
                }
              },
              required: [
                "command",
                "type"
              ],
              title: "UnknownCommandAction",
              type: "object"
            }
          ]
        },
        CommandExecutionSource: {
          enum: [
            "agent",
            "userShell",
            "unifiedExecStartup",
            "unifiedExecInteraction"
          ],
          type: "string"
        },
        CommandExecutionStatus: {
          enum: [
            "inProgress",
            "completed",
            "failed",
            "declined"
          ],
          type: "string"
        },
        DynamicToolCallOutputContentItem: {
          oneOf: [
            {
              properties: {
                text: {
                  type: "string"
                },
                type: {
                  enum: [
                    "inputText"
                  ],
                  title: "InputTextDynamicToolCallOutputContentItemType",
                  type: "string"
                }
              },
              required: [
                "text",
                "type"
              ],
              title: "InputTextDynamicToolCallOutputContentItem",
              type: "object"
            },
            {
              properties: {
                imageUrl: {
                  type: "string"
                },
                type: {
                  enum: [
                    "inputImage"
                  ],
                  title: "InputImageDynamicToolCallOutputContentItemType",
                  type: "string"
                }
              },
              required: [
                "imageUrl",
                "type"
              ],
              title: "InputImageDynamicToolCallOutputContentItem",
              type: "object"
            },
            {
              properties: {
                audioUrl: {
                  type: "string"
                },
                type: {
                  enum: [
                    "inputAudio"
                  ],
                  title: "InputAudioDynamicToolCallOutputContentItemType",
                  type: "string"
                }
              },
              required: [
                "audioUrl",
                "type"
              ],
              title: "InputAudioDynamicToolCallOutputContentItem",
              type: "object"
            }
          ]
        },
        DynamicToolCallStatus: {
          enum: [
            "inProgress",
            "completed",
            "failed"
          ],
          type: "string"
        },
        FileUpdateChange: {
          properties: {
            diff: {
              type: "string"
            },
            kind: {
              $ref: "#/definitions/PatchChangeKind"
            },
            path: {
              type: "string"
            }
          },
          required: [
            "diff",
            "kind",
            "path"
          ],
          type: "object"
        },
        FunctionCallOutputBody: {
          anyOf: [
            {
              type: "string"
            },
            {
              items: {
                $ref: "#/definitions/FunctionCallOutputContentItem"
              },
              type: "array"
            }
          ]
        },
        FunctionCallOutputContentItem: {
          description: "Responses API compatible content items that can be returned by a tool call. This is a subset of ContentItem with the types we support as function call outputs.",
          oneOf: [
            {
              properties: {
                text: {
                  type: "string"
                },
                type: {
                  enum: [
                    "input_text"
                  ],
                  title: "InputTextFunctionCallOutputContentItemType",
                  type: "string"
                }
              },
              required: [
                "text",
                "type"
              ],
              title: "InputTextFunctionCallOutputContentItem",
              type: "object"
            },
            {
              anyOf: [
                {
                  properties: {
                    image_url: {
                      type: "string"
                    }
                  },
                  required: [
                    "image_url"
                  ],
                  title: "ImageUrlFunctionCallOutputContentItem",
                  type: "object"
                },
                {
                  properties: {
                    file_id: {
                      type: "string"
                    }
                  },
                  required: [
                    "file_id"
                  ],
                  title: "FileIdFunctionCallOutputContentItem",
                  type: "object"
                }
              ],
              properties: {
                detail: {
                  anyOf: [
                    {
                      $ref: "#/definitions/ImageDetail"
                    },
                    {
                      type: "null"
                    }
                  ]
                },
                type: {
                  enum: [
                    "input_image"
                  ],
                  title: "InputImageFunctionCallOutputContentItemType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "InputImageFunctionCallOutputContentItem",
              type: "object"
            },
            {
              properties: {
                audio_url: {
                  type: "string"
                },
                type: {
                  enum: [
                    "input_audio"
                  ],
                  title: "InputAudioFunctionCallOutputContentItemType",
                  type: "string"
                }
              },
              required: [
                "audio_url",
                "type"
              ],
              title: "InputAudioFunctionCallOutputContentItem",
              type: "object"
            },
            {
              properties: {
                encrypted_content: {
                  type: "string"
                },
                type: {
                  enum: [
                    "encrypted_content"
                  ],
                  title: "EncryptedContentFunctionCallOutputContentItemType",
                  type: "string"
                }
              },
              required: [
                "encrypted_content",
                "type"
              ],
              title: "EncryptedContentFunctionCallOutputContentItem",
              type: "object"
            }
          ]
        },
        GitInfo: {
          properties: {
            branch: {
              type: [
                "string",
                "null"
              ]
            },
            originUrl: {
              type: [
                "string",
                "null"
              ]
            },
            sha: {
              type: [
                "string",
                "null"
              ]
            }
          },
          type: "object"
        },
        HookPromptFragment: {
          properties: {
            hookRunId: {
              type: "string"
            },
            text: {
              type: "string"
            }
          },
          required: [
            "hookRunId",
            "text"
          ],
          type: "object"
        },
        ImageDetail: {
          enum: [
            "auto",
            "low",
            "high",
            "original"
          ],
          type: "string"
        },
        ImageGenerationFailure: {
          oneOf: [
            {
              properties: {
                limitId: {
                  type: "string"
                },
                resetsAt: {
                  format: "int64",
                  type: [
                    "integer",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "usageLimitExceeded"
                  ],
                  title: "UsageLimitExceededImageGenerationFailureType",
                  type: "string"
                }
              },
              required: [
                "limitId",
                "type"
              ],
              title: "UsageLimitExceededImageGenerationFailure",
              type: "object"
            }
          ]
        },
        LegacyAppPathString: {
          type: "string"
        },
        McpAppDisplayMode: {
          enum: [
            "inline",
            "fullscreen"
          ],
          type: "string"
        },
        McpAppUi: {
          description: "UI resource and display preference for model invocations, captured from the tool descriptor.",
          properties: {
            preferredModelDisplayMode: {
              $ref: "#/definitions/McpAppDisplayMode"
            },
            resourceUri: {
              type: "string"
            }
          },
          required: [
            "preferredModelDisplayMode",
            "resourceUri"
          ],
          type: "object"
        },
        McpToolCallAppContext: {
          properties: {
            actionName: {
              type: [
                "string",
                "null"
              ]
            },
            appName: {
              type: [
                "string",
                "null"
              ]
            },
            connectorId: {
              type: "string"
            },
            linkId: {
              type: [
                "string",
                "null"
              ]
            },
            resourceUri: {
              type: [
                "string",
                "null"
              ]
            }
          },
          required: [
            "connectorId"
          ],
          type: "object"
        },
        McpToolCallError: {
          properties: {
            message: {
              type: "string"
            }
          },
          required: [
            "message"
          ],
          type: "object"
        },
        McpToolCallResult: {
          properties: {
            _meta: true,
            content: {
              items: true,
              type: "array"
            },
            structuredContent: true
          },
          required: [
            "content"
          ],
          type: "object"
        },
        McpToolCallStatus: {
          enum: [
            "inProgress",
            "completed",
            "failed"
          ],
          type: "string"
        },
        MemoryCitation: {
          properties: {
            entries: {
              items: {
                $ref: "#/definitions/MemoryCitationEntry"
              },
              type: "array"
            },
            threadIds: {
              items: {
                type: "string"
              },
              type: "array"
            }
          },
          required: [
            "entries",
            "threadIds"
          ],
          type: "object"
        },
        MemoryCitationEntry: {
          properties: {
            lineEnd: {
              format: "uint32",
              minimum: 0,
              type: "integer"
            },
            lineStart: {
              format: "uint32",
              minimum: 0,
              type: "integer"
            },
            note: {
              type: "string"
            },
            path: {
              type: "string"
            }
          },
          required: [
            "lineEnd",
            "lineStart",
            "note",
            "path"
          ],
          type: "object"
        },
        MessagePhase: {
          description: 'Classifies assistant text as commentary, a partial answer, or a terminal answer.\n\nProviders do not emit this consistently, so callers must treat `None` as "phase unknown" and keep compatibility behavior for legacy models.',
          oneOf: [
            {
              description: "Mid-turn assistant text (for example preamble/progress narration).\n\nAdditional tool calls or assistant output may follow before turn completion.",
              enum: [
                "commentary"
              ],
              type: "string"
            },
            {
              description: "Stable answer text that may be followed by more assistant output or tools.",
              enum: [
                "partial_answer"
              ],
              type: "string"
            },
            {
              description: "The assistant's declared terminal answer text for the current turn.",
              enum: [
                "final_answer"
              ],
              type: "string"
            }
          ]
        },
        MisalignmentErrorDetails: {
          properties: {
            detailedExplanation: {
              description: "A substantive localized explanation is required before offering continuation.",
              type: [
                "string",
                "null"
              ]
            },
            errorType: {
              description: "Open-ended classification; clients must accept categories added by Responses.",
              type: [
                "string",
                "null"
              ]
            },
            reviewTarget: {
              description: "Opaque server-issued block target. Presence alone does not enable target-based continuation.",
              type: [
                "string",
                "null"
              ]
            },
            steer: {
              anyOf: [
                {
                  $ref: "#/definitions/MisalignmentSteer"
                },
                {
                  type: "null"
                }
              ],
              description: "Instruction to submit as the next turn's user input if continuation is confirmed."
            }
          },
          type: "object"
        },
        MisalignmentSteer: {
          properties: {
            message: {
              type: "string"
            }
          },
          required: [
            "message"
          ],
          type: "object"
        },
        ModeKind: {
          description: "Initial collaboration mode to use when the TUI starts.",
          enum: [
            "plan",
            "default"
          ],
          type: "string"
        },
        MultiAgentMode: {
          description: "Controls the effective multi-agent delegation instructions for a turn. `custom` means the configured mode hint defines the policy instead of a built-in policy.",
          oneOf: [
            {
              enum: [
                "explicitRequestOnly",
                "proactive"
              ],
              type: "string"
            },
            {
              additionalProperties: false,
              properties: {
                custom: {
                  type: "string"
                }
              },
              required: [
                "custom"
              ],
              title: "CustomMultiAgentMode",
              type: "object"
            }
          ]
        },
        NetworkAccess: {
          enum: [
            "restricted",
            "enabled"
          ],
          type: "string"
        },
        NonSteerableTurnKind: {
          enum: [
            "review",
            "compact"
          ],
          type: "string"
        },
        PatchApplyStatus: {
          enum: [
            "inProgress",
            "completed",
            "failed",
            "declined"
          ],
          type: "string"
        },
        PatchChangeKind: {
          oneOf: [
            {
              properties: {
                type: {
                  enum: [
                    "add"
                  ],
                  title: "AddPatchChangeKindType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "AddPatchChangeKind",
              type: "object"
            },
            {
              properties: {
                type: {
                  enum: [
                    "delete"
                  ],
                  title: "DeletePatchChangeKindType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "DeletePatchChangeKind",
              type: "object"
            },
            {
              properties: {
                move_path: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "update"
                  ],
                  title: "UpdatePatchChangeKindType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "UpdatePatchChangeKind",
              type: "object"
            }
          ]
        },
        ReasoningEffort: {
          description: "A non-empty reasoning effort value advertised by the model.",
          minLength: 1,
          type: "string"
        },
        SandboxPolicy: {
          oneOf: [
            {
              properties: {
                type: {
                  enum: [
                    "dangerFullAccess"
                  ],
                  title: "DangerFullAccessSandboxPolicyType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "DangerFullAccessSandboxPolicy",
              type: "object"
            },
            {
              properties: {
                networkAccess: {
                  default: false,
                  type: "boolean"
                },
                type: {
                  enum: [
                    "readOnly"
                  ],
                  title: "ReadOnlySandboxPolicyType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "ReadOnlySandboxPolicy",
              type: "object"
            },
            {
              properties: {
                networkAccess: {
                  allOf: [
                    {
                      $ref: "#/definitions/NetworkAccess"
                    }
                  ],
                  default: "restricted"
                },
                type: {
                  enum: [
                    "externalSandbox"
                  ],
                  title: "ExternalSandboxSandboxPolicyType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "ExternalSandboxSandboxPolicy",
              type: "object"
            },
            {
              properties: {
                excludeSlashTmp: {
                  default: false,
                  type: "boolean"
                },
                excludeTmpdirEnvVar: {
                  default: false,
                  type: "boolean"
                },
                networkAccess: {
                  default: false,
                  type: "boolean"
                },
                type: {
                  enum: [
                    "workspaceWrite"
                  ],
                  title: "WorkspaceWriteSandboxPolicyType",
                  type: "string"
                },
                writableRoots: {
                  default: [],
                  items: {
                    $ref: "#/definitions/AbsolutePathBuf"
                  },
                  type: "array"
                }
              },
              required: [
                "type"
              ],
              title: "WorkspaceWriteSandboxPolicy",
              type: "object"
            }
          ]
        },
        SessionSource: {
          oneOf: [
            {
              enum: [
                "cli",
                "vscode",
                "exec",
                "appServer",
                "unknown"
              ],
              type: "string"
            },
            {
              additionalProperties: false,
              properties: {
                custom: {
                  type: "string"
                }
              },
              required: [
                "custom"
              ],
              title: "CustomSessionSource",
              type: "object"
            },
            {
              additionalProperties: false,
              properties: {
                subAgent: {
                  $ref: "#/definitions/SubAgentSource"
                }
              },
              required: [
                "subAgent"
              ],
              title: "SubAgentSessionSource",
              type: "object"
            }
          ]
        },
        Settings: {
          description: "Settings for a collaboration mode.",
          properties: {
            developer_instructions: {
              type: [
                "string",
                "null"
              ]
            },
            model: {
              type: "string"
            },
            reasoning_effort: {
              anyOf: [
                {
                  $ref: "#/definitions/ReasoningEffort"
                },
                {
                  type: "null"
                }
              ]
            }
          },
          required: [
            "model"
          ],
          type: "object"
        },
        SubAgentActivityKind: {
          enum: [
            "started",
            "interacted",
            "interrupted",
            "completed"
          ],
          type: "string"
        },
        SubAgentSource: {
          oneOf: [
            {
              enum: [
                "review",
                "compact",
                "memory_consolidation"
              ],
              type: "string"
            },
            {
              additionalProperties: false,
              properties: {
                thread_spawn: {
                  properties: {
                    agent_nickname: {
                      default: null,
                      type: [
                        "string",
                        "null"
                      ]
                    },
                    agent_path: {
                      anyOf: [
                        {
                          $ref: "#/definitions/AgentPath"
                        },
                        {
                          type: "null"
                        }
                      ],
                      default: null
                    },
                    agent_role: {
                      default: null,
                      type: [
                        "string",
                        "null"
                      ]
                    },
                    depth: {
                      format: "int32",
                      type: "integer"
                    },
                    parent_thread_id: {
                      $ref: "#/definitions/ThreadId"
                    }
                  },
                  required: [
                    "depth",
                    "parent_thread_id"
                  ],
                  type: "object"
                }
              },
              required: [
                "thread_spawn"
              ],
              title: "ThreadSpawnSubAgentSource",
              type: "object"
            },
            {
              additionalProperties: false,
              properties: {
                other: {
                  type: "string"
                }
              },
              required: [
                "other"
              ],
              title: "OtherSubAgentSource",
              type: "object"
            }
          ]
        },
        TextElement: {
          properties: {
            byteRange: {
              allOf: [
                {
                  $ref: "#/definitions/ByteRange"
                }
              ],
              description: "Byte range in the parent `text` buffer that this element occupies."
            },
            placeholder: {
              description: "Optional human-readable placeholder for the element, displayed in the UI.",
              type: [
                "string",
                "null"
              ]
            }
          },
          required: [
            "byteRange"
          ],
          type: "object"
        },
        Thread: {
          properties: {
            agentNickname: {
              description: "Optional random unique nickname assigned to an AgentControl-spawned sub-agent.",
              type: [
                "string",
                "null"
              ]
            },
            agentRole: {
              description: "Optional role (agent_role) assigned to an AgentControl-spawned sub-agent.",
              type: [
                "string",
                "null"
              ]
            },
            cliVersion: {
              description: "Version of the CLI that created the thread.",
              type: "string"
            },
            createdAt: {
              description: "Unix timestamp (in seconds) when the thread was created.",
              format: "int64",
              type: "integer"
            },
            cwd: {
              allOf: [
                {
                  $ref: "#/definitions/AbsolutePathBuf"
                }
              ],
              description: "Working directory captured for the thread."
            },
            ephemeral: {
              description: "Whether the thread is ephemeral and should not be materialized on disk.",
              type: "boolean"
            },
            forkedFromId: {
              description: "Source thread id when this thread was created by forking another thread.",
              type: [
                "string",
                "null"
              ]
            },
            gitInfo: {
              anyOf: [
                {
                  $ref: "#/definitions/GitInfo"
                },
                {
                  type: "null"
                }
              ],
              description: "Optional Git metadata captured when the thread was created."
            },
            historyMode: {
              allOf: [
                {
                  $ref: "#/definitions/ThreadHistoryMode"
                }
              ],
              default: "legacy",
              description: "Persisted thread history contract selected when this thread was created."
            },
            id: {
              description: "Identifier for this thread. Codex-generated thread IDs are UUIDv7.",
              type: "string"
            },
            model: {
              description: "Current configured model when loaded, otherwise the latest persisted model. Null when unavailable. This is not per-turn execution telemetry.",
              type: [
                "string",
                "null"
              ]
            },
            modelProvider: {
              description: "Model provider used for this thread (for example, 'openai').",
              type: "string"
            },
            name: {
              description: "Optional user-facing thread title.",
              type: [
                "string",
                "null"
              ]
            },
            originator: {
              description: "Originator recorded when the thread was created, independent of its current client or executor. Null when the recorded originator is unavailable.",
              type: [
                "string",
                "null"
              ]
            },
            parentThreadId: {
              description: "The ID of the parent thread. This will only be set if this thread is a subagent.",
              type: [
                "string",
                "null"
              ]
            },
            path: {
              description: "[UNSTABLE] Path to the thread on disk.",
              type: [
                "string",
                "null"
              ]
            },
            preview: {
              description: "Usually the first user message in the thread, if available.",
              type: "string"
            },
            projectId: {
              description: "Canonical project assignment owned by app-server, if any.",
              type: [
                "string",
                "null"
              ]
            },
            reasoningEffort: {
              anyOf: [
                {
                  $ref: "#/definitions/ReasoningEffort"
                },
                {
                  type: "null"
                }
              ],
              description: "Current configured reasoning effort when loaded, otherwise the latest persisted effort. Null when unset or unavailable. This is not per-turn execution telemetry."
            },
            recencyAt: {
              description: "Unix timestamp (in seconds) used for thread recency ordering.",
              format: "int64",
              type: [
                "integer",
                "null"
              ]
            },
            section: {
              anyOf: [
                {
                  $ref: "#/definitions/ThreadSection"
                },
                {
                  type: "null"
                }
              ],
              default: null,
              description: "The independently persisted section selected for this thread, if any."
            },
            sectionEnteredAt: {
              default: null,
              description: "Unix timestamp in seconds when the thread entered its current section.",
              format: "int64",
              type: [
                "integer",
                "null"
              ]
            },
            sessionId: {
              description: "Session id shared by threads that belong to the same session tree.",
              type: "string"
            },
            source: {
              allOf: [
                {
                  $ref: "#/definitions/SessionSource"
                }
              ],
              description: "Origin of the thread (CLI, VSCode, codex exec, codex app-server, etc.)."
            },
            status: {
              allOf: [
                {
                  $ref: "#/definitions/ThreadStatus"
                }
              ],
              description: "Current runtime status for the thread."
            },
            threadSource: {
              anyOf: [
                {
                  $ref: "#/definitions/ThreadSource"
                },
                {
                  type: "null"
                }
              ],
              description: "Optional analytics source classification for this thread."
            },
            turns: {
              description: "Only populated on `thread/resume`, `thread/fork`, and `thread/read` (when `includeTurns` is true) responses. For all other responses and notifications returning a Thread, the turns field will be an empty list.",
              items: {
                $ref: "#/definitions/Turn"
              },
              type: "array"
            },
            updatedAt: {
              description: "Unix timestamp (in seconds) when the thread was last updated.",
              format: "int64",
              type: "integer"
            }
          },
          required: [
            "cliVersion",
            "createdAt",
            "cwd",
            "ephemeral",
            "id",
            "modelProvider",
            "preview",
            "projectId",
            "sessionId",
            "source",
            "status",
            "turns",
            "updatedAt"
          ],
          type: "object"
        },
        ThreadActiveFlag: {
          enum: [
            "waitingOnApproval",
            "waitingOnUserInput"
          ],
          type: "string"
        },
        ThreadEnvironment: {
          description: "An environment selected by a loaded thread, independent of connection status.",
          properties: {
            cwd: {
              $ref: "#/definitions/LegacyAppPathString"
            },
            environmentId: {
              type: "string"
            },
            runtimeWorkspaceRoots: {
              items: {
                $ref: "#/definitions/LegacyAppPathString"
              },
              type: "array"
            }
          },
          required: [
            "cwd",
            "environmentId",
            "runtimeWorkspaceRoots"
          ],
          type: "object"
        },
        ThreadExtra: {
          description: "Extra app-server data for a thread.",
          type: "object"
        },
        ThreadHistoryMode: {
          enum: [
            "legacy",
            "paginated"
          ],
          type: "string"
        },
        ThreadId: {
          type: "string"
        },
        ThreadItem: {
          oneOf: [
            {
              properties: {
                clientId: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                content: {
                  items: {
                    $ref: "#/definitions/UserInput"
                  },
                  type: "array"
                },
                id: {
                  type: "string"
                },
                type: {
                  enum: [
                    "userMessage"
                  ],
                  title: "UserMessageThreadItemType",
                  type: "string"
                }
              },
              required: [
                "content",
                "id",
                "type"
              ],
              title: "UserMessageThreadItem",
              type: "object"
            },
            {
              properties: {
                fragments: {
                  items: {
                    $ref: "#/definitions/HookPromptFragment"
                  },
                  type: "array"
                },
                id: {
                  type: "string"
                },
                type: {
                  enum: [
                    "hookPrompt"
                  ],
                  title: "HookPromptThreadItemType",
                  type: "string"
                }
              },
              required: [
                "fragments",
                "id",
                "type"
              ],
              title: "HookPromptThreadItem",
              type: "object"
            },
            {
              properties: {
                delivery: {
                  anyOf: [
                    {
                      $ref: "#/definitions/AgentMessageDelivery"
                    },
                    {
                      type: "null"
                    }
                  ],
                  default: null
                },
                id: {
                  type: "string"
                },
                memoryCitation: {
                  anyOf: [
                    {
                      $ref: "#/definitions/MemoryCitation"
                    },
                    {
                      type: "null"
                    }
                  ],
                  default: null
                },
                phase: {
                  anyOf: [
                    {
                      $ref: "#/definitions/MessagePhase"
                    },
                    {
                      type: "null"
                    }
                  ],
                  default: null
                },
                questions: {
                  default: null,
                  items: {
                    $ref: "#/definitions/AsyncUserInputQuestion"
                  },
                  type: [
                    "array",
                    "null"
                  ]
                },
                text: {
                  type: "string"
                },
                type: {
                  enum: [
                    "agentMessage"
                  ],
                  title: "AgentMessageThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "text",
                "type"
              ],
              title: "AgentMessageThreadItem",
              type: "object"
            },
            {
              properties: {
                id: {
                  type: "string"
                },
                name: {
                  type: "string"
                },
                namespace: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                output: {
                  $ref: "#/definitions/FunctionCallOutputBody"
                },
                type: {
                  enum: [
                    "functionCallOutput"
                  ],
                  title: "FunctionCallOutputThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "name",
                "output",
                "type"
              ],
              title: "FunctionCallOutputThreadItem",
              type: "object"
            },
            {
              description: "EXPERIMENTAL - proposed plan item content. The completed plan item is authoritative and may not match the concatenation of `PlanDelta` text.",
              properties: {
                id: {
                  type: "string"
                },
                text: {
                  type: "string"
                },
                type: {
                  enum: [
                    "plan"
                  ],
                  title: "PlanThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "text",
                "type"
              ],
              title: "PlanThreadItem",
              type: "object"
            },
            {
              properties: {
                content: {
                  default: [],
                  items: {
                    type: "string"
                  },
                  type: "array"
                },
                id: {
                  type: "string"
                },
                summary: {
                  default: [],
                  items: {
                    type: "string"
                  },
                  type: "array"
                },
                type: {
                  enum: [
                    "reasoning"
                  ],
                  title: "ReasoningThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "type"
              ],
              title: "ReasoningThreadItem",
              type: "object"
            },
            {
              properties: {
                aggregatedOutput: {
                  description: "The command's output, aggregated from stdout and stderr.",
                  type: [
                    "string",
                    "null"
                  ]
                },
                command: {
                  description: "The command to be executed.",
                  type: "string"
                },
                commandActions: {
                  description: "A best-effort parsing of the command to understand the action(s) it will perform. This returns a list of CommandAction objects because a single shell command may be composed of many commands piped together.",
                  items: {
                    $ref: "#/definitions/CommandAction"
                  },
                  type: "array"
                },
                cwd: {
                  allOf: [
                    {
                      $ref: "#/definitions/LegacyAppPathString"
                    }
                  ],
                  description: "The command's working directory."
                },
                durationMs: {
                  description: "The duration of the command execution in milliseconds.",
                  format: "int64",
                  type: [
                    "integer",
                    "null"
                  ]
                },
                exitCode: {
                  description: "The command's exit code.",
                  format: "int32",
                  type: [
                    "integer",
                    "null"
                  ]
                },
                id: {
                  type: "string"
                },
                pluginId: {
                  default: null,
                  description: "Trusted first-party plugin id when this command resolves to one plugin script.",
                  type: [
                    "string",
                    "null"
                  ]
                },
                processId: {
                  description: "Identifier for the underlying PTY process (when available).",
                  type: [
                    "string",
                    "null"
                  ]
                },
                scriptPath: {
                  default: null,
                  description: "Safe plugin-relative path when this command resolves to one plugin script.",
                  type: [
                    "string",
                    "null"
                  ]
                },
                source: {
                  allOf: [
                    {
                      $ref: "#/definitions/CommandExecutionSource"
                    }
                  ],
                  default: "agent"
                },
                status: {
                  $ref: "#/definitions/CommandExecutionStatus"
                },
                type: {
                  enum: [
                    "commandExecution"
                  ],
                  title: "CommandExecutionThreadItemType",
                  type: "string"
                }
              },
              required: [
                "command",
                "commandActions",
                "cwd",
                "id",
                "status",
                "type"
              ],
              title: "CommandExecutionThreadItem",
              type: "object"
            },
            {
              properties: {
                changes: {
                  items: {
                    $ref: "#/definitions/FileUpdateChange"
                  },
                  type: "array"
                },
                id: {
                  type: "string"
                },
                status: {
                  $ref: "#/definitions/PatchApplyStatus"
                },
                type: {
                  enum: [
                    "fileChange"
                  ],
                  title: "FileChangeThreadItemType",
                  type: "string"
                }
              },
              required: [
                "changes",
                "id",
                "status",
                "type"
              ],
              title: "FileChangeThreadItem",
              type: "object"
            },
            {
              properties: {
                appContext: {
                  anyOf: [
                    {
                      $ref: "#/definitions/McpToolCallAppContext"
                    },
                    {
                      type: "null"
                    }
                  ]
                },
                arguments: true,
                durationMs: {
                  description: "The duration of the MCP tool call in milliseconds.",
                  format: "int64",
                  type: [
                    "integer",
                    "null"
                  ]
                },
                error: {
                  anyOf: [
                    {
                      $ref: "#/definitions/McpToolCallError"
                    },
                    {
                      type: "null"
                    }
                  ]
                },
                id: {
                  type: "string"
                },
                mcpAppResourceUri: {
                  description: "Legacy compatibility field; prefer `mcpAppUi.resourceUri` when available.",
                  type: [
                    "string",
                    "null"
                  ]
                },
                mcpAppUi: {
                  anyOf: [
                    {
                      $ref: "#/definitions/McpAppUi"
                    },
                    {
                      type: "null"
                    }
                  ],
                  description: "Presentation captured from the invoked descriptor; absent in older history."
                },
                pluginId: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                readOnlyHint: {
                  type: [
                    "boolean",
                    "null"
                  ]
                },
                result: {
                  anyOf: [
                    {
                      $ref: "#/definitions/McpToolCallResult"
                    },
                    {
                      type: "null"
                    }
                  ]
                },
                server: {
                  type: "string"
                },
                status: {
                  $ref: "#/definitions/McpToolCallStatus"
                },
                tool: {
                  type: "string"
                },
                type: {
                  enum: [
                    "mcpToolCall"
                  ],
                  title: "McpToolCallThreadItemType",
                  type: "string"
                }
              },
              required: [
                "arguments",
                "id",
                "server",
                "status",
                "tool",
                "type"
              ],
              title: "McpToolCallThreadItem",
              type: "object"
            },
            {
              properties: {
                arguments: true,
                contentItems: {
                  items: {
                    $ref: "#/definitions/DynamicToolCallOutputContentItem"
                  },
                  type: [
                    "array",
                    "null"
                  ]
                },
                durationMs: {
                  description: "The duration of the dynamic tool call in milliseconds.",
                  format: "int64",
                  type: [
                    "integer",
                    "null"
                  ]
                },
                id: {
                  type: "string"
                },
                namespace: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                status: {
                  $ref: "#/definitions/DynamicToolCallStatus"
                },
                success: {
                  type: [
                    "boolean",
                    "null"
                  ]
                },
                tool: {
                  type: "string"
                },
                type: {
                  enum: [
                    "dynamicToolCall"
                  ],
                  title: "DynamicToolCallThreadItemType",
                  type: "string"
                }
              },
              required: [
                "arguments",
                "id",
                "status",
                "tool",
                "type"
              ],
              title: "DynamicToolCallThreadItem",
              type: "object"
            },
            {
              properties: {
                agentsStates: {
                  additionalProperties: {
                    $ref: "#/definitions/CollabAgentState"
                  },
                  description: "Last known status of the target agents, when available.",
                  type: "object"
                },
                id: {
                  description: "Unique identifier for this collab tool call.",
                  type: "string"
                },
                model: {
                  description: "Model requested for the spawned agent, when applicable.",
                  type: [
                    "string",
                    "null"
                  ]
                },
                prompt: {
                  description: "Prompt text sent as part of the collab tool call, when available.",
                  type: [
                    "string",
                    "null"
                  ]
                },
                reasoningEffort: {
                  anyOf: [
                    {
                      $ref: "#/definitions/ReasoningEffort"
                    },
                    {
                      type: "null"
                    }
                  ],
                  description: "Reasoning effort requested for the spawned agent, when applicable."
                },
                receiverThreadIds: {
                  description: "Thread ID of the receiving agent, when applicable. In case of spawn operation, this corresponds to the newly spawned agent.",
                  items: {
                    type: "string"
                  },
                  type: "array"
                },
                senderThreadId: {
                  description: "Thread ID of the agent issuing the collab request.",
                  type: "string"
                },
                status: {
                  allOf: [
                    {
                      $ref: "#/definitions/CollabAgentToolCallStatus"
                    }
                  ],
                  description: "Current status of the collab tool call."
                },
                tool: {
                  allOf: [
                    {
                      $ref: "#/definitions/CollabAgentTool"
                    }
                  ],
                  description: "Name of the collab tool that was invoked."
                },
                type: {
                  enum: [
                    "collabAgentToolCall"
                  ],
                  title: "CollabAgentToolCallThreadItemType",
                  type: "string"
                }
              },
              required: [
                "agentsStates",
                "id",
                "receiverThreadIds",
                "senderThreadId",
                "status",
                "tool",
                "type"
              ],
              title: "CollabAgentToolCallThreadItem",
              type: "object"
            },
            {
              properties: {
                agentPath: {
                  type: "string"
                },
                agentThreadId: {
                  type: "string"
                },
                id: {
                  type: "string"
                },
                kind: {
                  $ref: "#/definitions/SubAgentActivityKind"
                },
                model: {
                  description: "Resolved model at sub-agent creation; absent from older records and other activities.",
                  type: [
                    "string",
                    "null"
                  ]
                },
                reasoningEffort: {
                  anyOf: [
                    {
                      $ref: "#/definitions/ReasoningEffort"
                    },
                    {
                      type: "null"
                    }
                  ],
                  description: "Resolved reasoning effort at sub-agent creation, when known."
                },
                type: {
                  enum: [
                    "subAgentActivity"
                  ],
                  title: "SubAgentActivityThreadItemType",
                  type: "string"
                }
              },
              required: [
                "agentPath",
                "agentThreadId",
                "id",
                "kind",
                "type"
              ],
              title: "SubAgentActivityThreadItem",
              type: "object"
            },
            {
              properties: {
                action: {
                  anyOf: [
                    {
                      $ref: "#/definitions/WebSearchAction"
                    },
                    {
                      type: "null"
                    }
                  ]
                },
                id: {
                  type: "string"
                },
                query: {
                  type: "string"
                },
                results: {
                  default: null,
                  description: "Structured search results returned out-of-band by standalone web search.\n\nThese stay as opaque JSON at the extension/app-server boundary so new result fields and result types can pass through without a Codex release.",
                  items: true,
                  type: [
                    "array",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "webSearch"
                  ],
                  title: "WebSearchThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "query",
                "type"
              ],
              title: "WebSearchThreadItem",
              type: "object"
            },
            {
              properties: {
                id: {
                  type: "string"
                },
                path: {
                  $ref: "#/definitions/LegacyAppPathString"
                },
                type: {
                  enum: [
                    "imageView"
                  ],
                  title: "ImageViewThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "path",
                "type"
              ],
              title: "ImageViewThreadItem",
              type: "object"
            },
            {
              description: "Display item emitted by the interruptible `clock.sleep` tool.",
              properties: {
                durationMs: {
                  format: "uint64",
                  minimum: 0,
                  type: "integer"
                },
                id: {
                  type: "string"
                },
                type: {
                  enum: [
                    "sleep"
                  ],
                  title: "SleepThreadItemType",
                  type: "string"
                }
              },
              required: [
                "durationMs",
                "id",
                "type"
              ],
              title: "SleepThreadItem",
              type: "object"
            },
            {
              properties: {
                failure: {
                  anyOf: [
                    {
                      $ref: "#/definitions/ImageGenerationFailure"
                    },
                    {
                      type: "null"
                    }
                  ],
                  default: null
                },
                id: {
                  type: "string"
                },
                result: {
                  type: "string"
                },
                revisedPrompt: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                savedPath: {
                  anyOf: [
                    {
                      $ref: "#/definitions/AbsolutePathBuf"
                    },
                    {
                      type: "null"
                    }
                  ]
                },
                status: {
                  type: "string"
                },
                transparentBackground: {
                  default: null,
                  type: [
                    "boolean",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "imageGeneration"
                  ],
                  title: "ImageGenerationThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "result",
                "status",
                "type"
              ],
              title: "ImageGenerationThreadItem",
              type: "object"
            },
            {
              properties: {
                id: {
                  type: "string"
                },
                review: {
                  type: "string"
                },
                type: {
                  enum: [
                    "enteredReviewMode"
                  ],
                  title: "EnteredReviewModeThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "review",
                "type"
              ],
              title: "EnteredReviewModeThreadItem",
              type: "object"
            },
            {
              properties: {
                id: {
                  type: "string"
                },
                review: {
                  type: "string"
                },
                type: {
                  enum: [
                    "exitedReviewMode"
                  ],
                  title: "ExitedReviewModeThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "review",
                "type"
              ],
              title: "ExitedReviewModeThreadItem",
              type: "object"
            },
            {
              properties: {
                id: {
                  type: "string"
                },
                type: {
                  enum: [
                    "contextCompaction"
                  ],
                  title: "ContextCompactionThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "type"
              ],
              title: "ContextCompactionThreadItem",
              type: "object"
            }
          ]
        },
        ThreadSection: {
          description: "An independently persisted, user-visible thread section.",
          properties: {
            appearance: {
              anyOf: [
                {
                  $ref: "#/definitions/ThreadSectionAppearance"
                },
                {
                  type: "null"
                }
              ],
              default: null,
              description: "Optional appearance synchronized across clients."
            },
            id: {
              description: "Opaque UUIDv7 identity that remains stable when the section is renamed.",
              type: "string"
            },
            name: {
              description: "The current user-visible section name.",
              type: "string"
            }
          },
          required: [
            "id",
            "name"
          ],
          type: "object"
        },
        ThreadSectionAppearance: {
          description: "Extensible visual presentation for a custom thread section.",
          properties: {
            color: {
              type: [
                "string",
                "null"
              ]
            },
            icon: {
              type: [
                "string",
                "null"
              ]
            }
          },
          type: "object"
        },
        ThreadSource: {
          type: "string"
        },
        ThreadStatus: {
          oneOf: [
            {
              properties: {
                type: {
                  enum: [
                    "notLoaded"
                  ],
                  title: "NotLoadedThreadStatusType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "NotLoadedThreadStatus",
              type: "object"
            },
            {
              properties: {
                type: {
                  enum: [
                    "idle"
                  ],
                  title: "IdleThreadStatusType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "IdleThreadStatus",
              type: "object"
            },
            {
              properties: {
                type: {
                  enum: [
                    "systemError"
                  ],
                  title: "SystemErrorThreadStatusType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "SystemErrorThreadStatus",
              type: "object"
            },
            {
              properties: {
                activeFlags: {
                  items: {
                    $ref: "#/definitions/ThreadActiveFlag"
                  },
                  type: "array"
                },
                type: {
                  enum: [
                    "active"
                  ],
                  title: "ActiveThreadStatusType",
                  type: "string"
                }
              },
              required: [
                "activeFlags",
                "type"
              ],
              title: "ActiveThreadStatus",
              type: "object"
            }
          ]
        },
        Turn: {
          properties: {
            completedAt: {
              description: "Unix timestamp (in seconds) when the turn completed.",
              format: "int64",
              type: [
                "integer",
                "null"
              ]
            },
            durationMs: {
              description: "Duration between turn start and completion in milliseconds, if known.",
              format: "int64",
              type: [
                "integer",
                "null"
              ]
            },
            error: {
              anyOf: [
                {
                  $ref: "#/definitions/TurnError"
                },
                {
                  type: "null"
                }
              ],
              description: "Error associated with a failed or interrupted turn."
            },
            id: {
              description: "Identifier for this turn. Codex-generated turn IDs are UUIDv7.",
              type: "string"
            },
            items: {
              description: "Thread items currently included in this turn payload.",
              items: {
                $ref: "#/definitions/ThreadItem"
              },
              type: "array"
            },
            itemsView: {
              allOf: [
                {
                  $ref: "#/definitions/TurnItemsView"
                }
              ],
              default: "full",
              description: "Describes how much of `items` has been loaded for this turn."
            },
            rootTurnId: {
              description: "ID of the first turn in the chain of work that led to this turn. Pass this as `rootTurnId` when starting work on behalf of this turn. May be null in older history or a `review/start` response.",
              type: [
                "string",
                "null"
              ]
            },
            startedAt: {
              description: "Unix timestamp (in seconds) when the turn started.",
              format: "int64",
              type: [
                "integer",
                "null"
              ]
            },
            status: {
              $ref: "#/definitions/TurnStatus"
            }
          },
          required: [
            "id",
            "items",
            "status"
          ],
          type: "object"
        },
        TurnError: {
          properties: {
            additionalDetails: {
              default: null,
              type: [
                "string",
                "null"
              ]
            },
            codexErrorInfo: {
              anyOf: [
                {
                  $ref: "#/definitions/CodexErrorInfo"
                },
                {
                  type: "null"
                }
              ]
            },
            message: {
              type: "string"
            },
            misalignment: {
              anyOf: [
                {
                  $ref: "#/definitions/MisalignmentErrorDetails"
                },
                {
                  type: "null"
                }
              ],
              default: null,
              description: "Optional public explanation and continuation instruction for a misalignment block."
            }
          },
          required: [
            "message"
          ],
          type: "object"
        },
        TurnItemsView: {
          oneOf: [
            {
              description: "`items` was not loaded for this turn. The field is intentionally empty.",
              enum: [
                "notLoaded"
              ],
              type: "string"
            },
            {
              description: "`items` contains only a display summary for this turn.",
              enum: [
                "summary"
              ],
              type: "string"
            },
            {
              description: "`items` contains every ThreadItem available from persisted app-server history for this turn.",
              enum: [
                "full"
              ],
              type: "string"
            }
          ]
        },
        TurnStatus: {
          enum: [
            "completed",
            "interrupted",
            "failed",
            "inProgress"
          ],
          type: "string"
        },
        TurnsPage: {
          properties: {
            backwardsCursor: {
              type: [
                "string",
                "null"
              ]
            },
            data: {
              items: {
                $ref: "#/definitions/Turn"
              },
              type: "array"
            },
            nextCursor: {
              type: [
                "string",
                "null"
              ]
            }
          },
          required: [
            "data"
          ],
          type: "object"
        },
        UserInput: {
          oneOf: [
            {
              properties: {
                text: {
                  type: "string"
                },
                text_elements: {
                  default: [],
                  description: "UI-defined spans within `text` used to render or persist special elements.",
                  items: {
                    $ref: "#/definitions/TextElement"
                  },
                  type: "array"
                },
                type: {
                  enum: [
                    "text"
                  ],
                  title: "TextUserInputType",
                  type: "string"
                }
              },
              required: [
                "text",
                "type"
              ],
              title: "TextUserInput",
              type: "object"
            },
            {
              anyOf: [
                {
                  properties: {
                    url: {
                      type: "string"
                    }
                  },
                  required: [
                    "url"
                  ],
                  title: "UrlUserInput",
                  type: "object"
                },
                {
                  properties: {
                    fileId: {
                      type: "string"
                    }
                  },
                  required: [
                    "fileId"
                  ],
                  title: "FileIdUserInput",
                  type: "object"
                }
              ],
              properties: {
                detail: {
                  anyOf: [
                    {
                      $ref: "#/definitions/ImageDetail"
                    },
                    {
                      type: "null"
                    }
                  ],
                  default: null
                },
                type: {
                  enum: [
                    "image"
                  ],
                  title: "ImageUserInputType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "ImageUserInput",
              type: "object"
            },
            {
              properties: {
                detail: {
                  anyOf: [
                    {
                      $ref: "#/definitions/ImageDetail"
                    },
                    {
                      type: "null"
                    }
                  ],
                  default: null
                },
                path: {
                  type: "string"
                },
                type: {
                  enum: [
                    "localImage"
                  ],
                  title: "LocalImageUserInputType",
                  type: "string"
                }
              },
              required: [
                "path",
                "type"
              ],
              title: "LocalImageUserInput",
              type: "object"
            },
            {
              properties: {
                type: {
                  enum: [
                    "audio"
                  ],
                  title: "AudioUserInputType",
                  type: "string"
                },
                url: {
                  type: "string"
                }
              },
              required: [
                "type",
                "url"
              ],
              title: "AudioUserInput",
              type: "object"
            },
            {
              properties: {
                path: {
                  type: "string"
                },
                type: {
                  enum: [
                    "localAudio"
                  ],
                  title: "LocalAudioUserInputType",
                  type: "string"
                }
              },
              required: [
                "path",
                "type"
              ],
              title: "LocalAudioUserInput",
              type: "object"
            },
            {
              properties: {
                name: {
                  type: "string"
                },
                path: {
                  type: "string"
                },
                type: {
                  enum: [
                    "skill"
                  ],
                  title: "SkillUserInputType",
                  type: "string"
                }
              },
              required: [
                "name",
                "path",
                "type"
              ],
              title: "SkillUserInput",
              type: "object"
            },
            {
              properties: {
                name: {
                  type: "string"
                },
                path: {
                  type: "string"
                },
                type: {
                  enum: [
                    "mention"
                  ],
                  title: "MentionUserInputType",
                  type: "string"
                }
              },
              required: [
                "name",
                "path",
                "type"
              ],
              title: "MentionUserInput",
              type: "object"
            }
          ]
        },
        WebSearchAction: {
          oneOf: [
            {
              properties: {
                queries: {
                  items: {
                    type: "string"
                  },
                  type: [
                    "array",
                    "null"
                  ]
                },
                query: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "search"
                  ],
                  title: "SearchWebSearchActionType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "SearchWebSearchAction",
              type: "object"
            },
            {
              properties: {
                type: {
                  enum: [
                    "openPage"
                  ],
                  title: "OpenPageWebSearchActionType",
                  type: "string"
                },
                url: {
                  type: [
                    "string",
                    "null"
                  ]
                }
              },
              required: [
                "type"
              ],
              title: "OpenPageWebSearchAction",
              type: "object"
            },
            {
              properties: {
                pattern: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "findInPage"
                  ],
                  title: "FindInPageWebSearchActionType",
                  type: "string"
                },
                url: {
                  type: [
                    "string",
                    "null"
                  ]
                }
              },
              required: [
                "type"
              ],
              title: "FindInPageWebSearchAction",
              type: "object"
            },
            {
              properties: {
                type: {
                  enum: [
                    "other"
                  ],
                  title: "OtherWebSearchActionType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "OtherWebSearchAction",
              type: "object"
            }
          ]
        }
      },
      properties: {
        approvalPolicy: {
          $ref: "#/definitions/AskForApproval"
        },
        approvalsReviewer: {
          allOf: [
            {
              $ref: "#/definitions/ApprovalsReviewer"
            }
          ],
          description: "Reviewer currently used for approval requests on this thread."
        },
        collaborationMode: {
          anyOf: [
            {
              $ref: "#/definitions/CollaborationMode"
            },
            {
              type: "null"
            }
          ],
          description: "Effective collaboration mode. Absent when resuming from an older server."
        },
        cwd: {
          $ref: "#/definitions/AbsolutePathBuf"
        },
        disabledPluginIds: {
          default: [],
          description: "Saved list of disabled plugin IDs. Does not yet filter plugin capabilities.",
          items: {
            type: "string"
          },
          type: "array"
        },
        instructionSources: {
          default: [],
          description: "Environment-native paths to instruction source files currently loaded for this thread.",
          items: {
            $ref: "#/definitions/LegacyAppPathString"
          },
          type: "array"
        },
        itemsBackwardsCursor: {
          default: null,
          description: 'Opaque cursor for hydrating paginated items backwards.\n\nPass this as `cursor` to `thread/items/list` with `sortDirection: "desc"`. The first page includes the item identified by the cursor.',
          type: [
            "string",
            "null"
          ]
        },
        model: {
          type: "string"
        },
        modelProvider: {
          type: "string"
        },
        reasoningEffort: {
          anyOf: [
            {
              $ref: "#/definitions/ReasoningEffort"
            },
            {
              type: "null"
            }
          ]
        },
        sandbox: {
          allOf: [
            {
              $ref: "#/definitions/SandboxPolicy"
            }
          ],
          description: "Legacy sandbox policy retained for compatibility. Experimental clients should prefer `activePermissionProfile` for profile provenance."
        },
        serviceTier: {
          type: [
            "string",
            "null"
          ]
        },
        thread: {
          $ref: "#/definitions/Thread"
        },
        turnsBackwardsCursor: {
          default: null,
          description: 'Opaque cursor for hydrating paginated turns backwards.\n\nPass this as `cursor` to `thread/turns/list` with `sortDirection: "desc"`. The first page includes the turn identified by the cursor.',
          type: [
            "string",
            "null"
          ]
        }
      },
      required: [
        "approvalPolicy",
        "approvalsReviewer",
        "cwd",
        "model",
        "modelProvider",
        "sandbox",
        "thread"
      ],
      title: "ThreadResumeResponse",
      type: "object"
    };
  }
});

// src/codex-protocol/schema/v2/TurnStartResponse.json
var TurnStartResponse_default;
var init_TurnStartResponse = __esm({
  "src/codex-protocol/schema/v2/TurnStartResponse.json"() {
    TurnStartResponse_default = {
      $schema: "http://json-schema.org/draft-07/schema#",
      definitions: {
        AbsolutePathBuf: {
          description: "A path that is guaranteed to be absolute and normalized (though it is not guaranteed to be canonicalized or exist on the filesystem).\n\nIMPORTANT: When deserializing an `AbsolutePathBuf`, a base path must be set using [AbsolutePathBufGuard::new]. If no base path is set, the deserialization will fail unless the path being deserialized is already absolute.",
          type: "string"
        },
        AgentMessageDelivery: {
          enum: [
            "async"
          ],
          type: "string"
        },
        AsyncUserInputQuestion: {
          additionalProperties: false,
          properties: {
            options: {
              items: {
                type: "string"
              },
              type: [
                "array",
                "null"
              ]
            },
            title: {
              type: "string"
            }
          },
          required: [
            "title"
          ],
          type: "object"
        },
        ByteRange: {
          properties: {
            end: {
              format: "uint",
              minimum: 0,
              type: "integer"
            },
            start: {
              format: "uint",
              minimum: 0,
              type: "integer"
            }
          },
          required: [
            "end",
            "start"
          ],
          type: "object"
        },
        CodexErrorInfo: {
          anyOf: [
            {
              enum: [
                "contextWindowExceeded",
                "sessionBudgetExceeded",
                "usageLimitExceeded",
                "rateLimitExceeded",
                "flexUnavailable",
                "serverOverloaded",
                "cyberPolicy",
                "misalignmentPolicyViolation",
                "tooManyDenials",
                "internalServerError",
                "unauthorized",
                "badRequest",
                "threadRollbackFailed",
                "sandboxError",
                "other"
              ],
              type: "string"
            },
            {
              additionalProperties: false,
              properties: {
                httpConnectionFailed: {
                  properties: {
                    httpStatusCode: {
                      format: "uint16",
                      minimum: 0,
                      type: [
                        "integer",
                        "null"
                      ]
                    }
                  },
                  type: "object"
                }
              },
              required: [
                "httpConnectionFailed"
              ],
              title: "HttpConnectionFailedCodexErrorInfo",
              type: "object"
            },
            {
              additionalProperties: false,
              description: "Failed to connect to the response SSE stream.",
              properties: {
                responseStreamConnectionFailed: {
                  properties: {
                    httpStatusCode: {
                      format: "uint16",
                      minimum: 0,
                      type: [
                        "integer",
                        "null"
                      ]
                    }
                  },
                  type: "object"
                }
              },
              required: [
                "responseStreamConnectionFailed"
              ],
              title: "ResponseStreamConnectionFailedCodexErrorInfo",
              type: "object"
            },
            {
              additionalProperties: false,
              description: "The response SSE stream disconnected in the middle of a turn before completion.",
              properties: {
                responseStreamDisconnected: {
                  properties: {
                    httpStatusCode: {
                      format: "uint16",
                      minimum: 0,
                      type: [
                        "integer",
                        "null"
                      ]
                    }
                  },
                  type: "object"
                }
              },
              required: [
                "responseStreamDisconnected"
              ],
              title: "ResponseStreamDisconnectedCodexErrorInfo",
              type: "object"
            },
            {
              additionalProperties: false,
              description: "Reached the retry limit for responses.",
              properties: {
                responseTooManyFailedAttempts: {
                  properties: {
                    httpStatusCode: {
                      format: "uint16",
                      minimum: 0,
                      type: [
                        "integer",
                        "null"
                      ]
                    }
                  },
                  type: "object"
                }
              },
              required: [
                "responseTooManyFailedAttempts"
              ],
              title: "ResponseTooManyFailedAttemptsCodexErrorInfo",
              type: "object"
            },
            {
              additionalProperties: false,
              description: "Returned when `turn/start` or `turn/steer` is submitted while the current active turn cannot accept same-turn steering, for example `/review` or manual `/compact`.",
              properties: {
                activeTurnNotSteerable: {
                  properties: {
                    turnKind: {
                      $ref: "#/definitions/NonSteerableTurnKind"
                    }
                  },
                  required: [
                    "turnKind"
                  ],
                  type: "object"
                }
              },
              required: [
                "activeTurnNotSteerable"
              ],
              title: "ActiveTurnNotSteerableCodexErrorInfo",
              type: "object"
            },
            {
              type: [
                "string",
                "object"
              ]
            }
          ],
          description: "This translation layer make sure that we expose codex error code in camel case.\n\nWhen an upstream HTTP status is available (for example, from the Responses API or a provider), it is forwarded in `httpStatusCode` on the relevant `codexErrorInfo` variant."
        },
        CollabAgentState: {
          properties: {
            message: {
              type: [
                "string",
                "null"
              ]
            },
            status: {
              $ref: "#/definitions/CollabAgentStatus"
            }
          },
          required: [
            "status"
          ],
          type: "object"
        },
        CollabAgentStatus: {
          enum: [
            "pendingInit",
            "running",
            "interrupted",
            "completed",
            "errored",
            "shutdown",
            "notFound"
          ],
          type: "string"
        },
        CollabAgentTool: {
          enum: [
            "spawnAgent",
            "sendInput",
            "resumeAgent",
            "wait",
            "closeAgent",
            "sendMessage",
            "followupTask",
            "interruptAgent",
            "listAgents"
          ],
          type: "string"
        },
        CollabAgentToolCallStatus: {
          enum: [
            "inProgress",
            "completed",
            "failed",
            "interrupted"
          ],
          type: "string"
        },
        CommandAction: {
          oneOf: [
            {
              properties: {
                command: {
                  type: "string"
                },
                name: {
                  type: "string"
                },
                path: {
                  $ref: "#/definitions/LegacyAppPathString"
                },
                type: {
                  enum: [
                    "read"
                  ],
                  title: "ReadCommandActionType",
                  type: "string"
                }
              },
              required: [
                "command",
                "name",
                "path",
                "type"
              ],
              title: "ReadCommandAction",
              type: "object"
            },
            {
              properties: {
                command: {
                  type: "string"
                },
                path: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "listFiles"
                  ],
                  title: "ListFilesCommandActionType",
                  type: "string"
                }
              },
              required: [
                "command",
                "type"
              ],
              title: "ListFilesCommandAction",
              type: "object"
            },
            {
              properties: {
                command: {
                  type: "string"
                },
                path: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                query: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "search"
                  ],
                  title: "SearchCommandActionType",
                  type: "string"
                }
              },
              required: [
                "command",
                "type"
              ],
              title: "SearchCommandAction",
              type: "object"
            },
            {
              properties: {
                command: {
                  type: "string"
                },
                type: {
                  enum: [
                    "unknown"
                  ],
                  title: "UnknownCommandActionType",
                  type: "string"
                }
              },
              required: [
                "command",
                "type"
              ],
              title: "UnknownCommandAction",
              type: "object"
            }
          ]
        },
        CommandExecutionSource: {
          enum: [
            "agent",
            "userShell",
            "unifiedExecStartup",
            "unifiedExecInteraction"
          ],
          type: "string"
        },
        CommandExecutionStatus: {
          enum: [
            "inProgress",
            "completed",
            "failed",
            "declined"
          ],
          type: "string"
        },
        DynamicToolCallOutputContentItem: {
          oneOf: [
            {
              properties: {
                text: {
                  type: "string"
                },
                type: {
                  enum: [
                    "inputText"
                  ],
                  title: "InputTextDynamicToolCallOutputContentItemType",
                  type: "string"
                }
              },
              required: [
                "text",
                "type"
              ],
              title: "InputTextDynamicToolCallOutputContentItem",
              type: "object"
            },
            {
              properties: {
                imageUrl: {
                  type: "string"
                },
                type: {
                  enum: [
                    "inputImage"
                  ],
                  title: "InputImageDynamicToolCallOutputContentItemType",
                  type: "string"
                }
              },
              required: [
                "imageUrl",
                "type"
              ],
              title: "InputImageDynamicToolCallOutputContentItem",
              type: "object"
            },
            {
              properties: {
                audioUrl: {
                  type: "string"
                },
                type: {
                  enum: [
                    "inputAudio"
                  ],
                  title: "InputAudioDynamicToolCallOutputContentItemType",
                  type: "string"
                }
              },
              required: [
                "audioUrl",
                "type"
              ],
              title: "InputAudioDynamicToolCallOutputContentItem",
              type: "object"
            }
          ]
        },
        DynamicToolCallStatus: {
          enum: [
            "inProgress",
            "completed",
            "failed"
          ],
          type: "string"
        },
        FileUpdateChange: {
          properties: {
            diff: {
              type: "string"
            },
            kind: {
              $ref: "#/definitions/PatchChangeKind"
            },
            path: {
              type: "string"
            }
          },
          required: [
            "diff",
            "kind",
            "path"
          ],
          type: "object"
        },
        FunctionCallOutputBody: {
          anyOf: [
            {
              type: "string"
            },
            {
              items: {
                $ref: "#/definitions/FunctionCallOutputContentItem"
              },
              type: "array"
            }
          ]
        },
        FunctionCallOutputContentItem: {
          description: "Responses API compatible content items that can be returned by a tool call. This is a subset of ContentItem with the types we support as function call outputs.",
          oneOf: [
            {
              properties: {
                text: {
                  type: "string"
                },
                type: {
                  enum: [
                    "input_text"
                  ],
                  title: "InputTextFunctionCallOutputContentItemType",
                  type: "string"
                }
              },
              required: [
                "text",
                "type"
              ],
              title: "InputTextFunctionCallOutputContentItem",
              type: "object"
            },
            {
              anyOf: [
                {
                  properties: {
                    image_url: {
                      type: "string"
                    }
                  },
                  required: [
                    "image_url"
                  ],
                  title: "ImageUrlFunctionCallOutputContentItem",
                  type: "object"
                },
                {
                  properties: {
                    file_id: {
                      type: "string"
                    }
                  },
                  required: [
                    "file_id"
                  ],
                  title: "FileIdFunctionCallOutputContentItem",
                  type: "object"
                }
              ],
              properties: {
                detail: {
                  anyOf: [
                    {
                      $ref: "#/definitions/ImageDetail"
                    },
                    {
                      type: "null"
                    }
                  ]
                },
                type: {
                  enum: [
                    "input_image"
                  ],
                  title: "InputImageFunctionCallOutputContentItemType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "InputImageFunctionCallOutputContentItem",
              type: "object"
            },
            {
              properties: {
                audio_url: {
                  type: "string"
                },
                type: {
                  enum: [
                    "input_audio"
                  ],
                  title: "InputAudioFunctionCallOutputContentItemType",
                  type: "string"
                }
              },
              required: [
                "audio_url",
                "type"
              ],
              title: "InputAudioFunctionCallOutputContentItem",
              type: "object"
            },
            {
              properties: {
                encrypted_content: {
                  type: "string"
                },
                type: {
                  enum: [
                    "encrypted_content"
                  ],
                  title: "EncryptedContentFunctionCallOutputContentItemType",
                  type: "string"
                }
              },
              required: [
                "encrypted_content",
                "type"
              ],
              title: "EncryptedContentFunctionCallOutputContentItem",
              type: "object"
            }
          ]
        },
        HookPromptFragment: {
          properties: {
            hookRunId: {
              type: "string"
            },
            text: {
              type: "string"
            }
          },
          required: [
            "hookRunId",
            "text"
          ],
          type: "object"
        },
        ImageDetail: {
          enum: [
            "auto",
            "low",
            "high",
            "original"
          ],
          type: "string"
        },
        ImageGenerationFailure: {
          oneOf: [
            {
              properties: {
                limitId: {
                  type: "string"
                },
                resetsAt: {
                  format: "int64",
                  type: [
                    "integer",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "usageLimitExceeded"
                  ],
                  title: "UsageLimitExceededImageGenerationFailureType",
                  type: "string"
                }
              },
              required: [
                "limitId",
                "type"
              ],
              title: "UsageLimitExceededImageGenerationFailure",
              type: "object"
            }
          ]
        },
        LegacyAppPathString: {
          type: "string"
        },
        McpAppDisplayMode: {
          enum: [
            "inline",
            "fullscreen"
          ],
          type: "string"
        },
        McpAppUi: {
          description: "UI resource and display preference for model invocations, captured from the tool descriptor.",
          properties: {
            preferredModelDisplayMode: {
              $ref: "#/definitions/McpAppDisplayMode"
            },
            resourceUri: {
              type: "string"
            }
          },
          required: [
            "preferredModelDisplayMode",
            "resourceUri"
          ],
          type: "object"
        },
        McpToolCallAppContext: {
          properties: {
            actionName: {
              type: [
                "string",
                "null"
              ]
            },
            appName: {
              type: [
                "string",
                "null"
              ]
            },
            connectorId: {
              type: "string"
            },
            linkId: {
              type: [
                "string",
                "null"
              ]
            },
            resourceUri: {
              type: [
                "string",
                "null"
              ]
            }
          },
          required: [
            "connectorId"
          ],
          type: "object"
        },
        McpToolCallError: {
          properties: {
            message: {
              type: "string"
            }
          },
          required: [
            "message"
          ],
          type: "object"
        },
        McpToolCallResult: {
          properties: {
            _meta: true,
            content: {
              items: true,
              type: "array"
            },
            structuredContent: true
          },
          required: [
            "content"
          ],
          type: "object"
        },
        McpToolCallStatus: {
          enum: [
            "inProgress",
            "completed",
            "failed"
          ],
          type: "string"
        },
        MemoryCitation: {
          properties: {
            entries: {
              items: {
                $ref: "#/definitions/MemoryCitationEntry"
              },
              type: "array"
            },
            threadIds: {
              items: {
                type: "string"
              },
              type: "array"
            }
          },
          required: [
            "entries",
            "threadIds"
          ],
          type: "object"
        },
        MemoryCitationEntry: {
          properties: {
            lineEnd: {
              format: "uint32",
              minimum: 0,
              type: "integer"
            },
            lineStart: {
              format: "uint32",
              minimum: 0,
              type: "integer"
            },
            note: {
              type: "string"
            },
            path: {
              type: "string"
            }
          },
          required: [
            "lineEnd",
            "lineStart",
            "note",
            "path"
          ],
          type: "object"
        },
        MessagePhase: {
          description: 'Classifies assistant text as commentary, a partial answer, or a terminal answer.\n\nProviders do not emit this consistently, so callers must treat `None` as "phase unknown" and keep compatibility behavior for legacy models.',
          oneOf: [
            {
              description: "Mid-turn assistant text (for example preamble/progress narration).\n\nAdditional tool calls or assistant output may follow before turn completion.",
              enum: [
                "commentary"
              ],
              type: "string"
            },
            {
              description: "Stable answer text that may be followed by more assistant output or tools.",
              enum: [
                "partial_answer"
              ],
              type: "string"
            },
            {
              description: "The assistant's declared terminal answer text for the current turn.",
              enum: [
                "final_answer"
              ],
              type: "string"
            }
          ]
        },
        MisalignmentErrorDetails: {
          properties: {
            detailedExplanation: {
              description: "A substantive localized explanation is required before offering continuation.",
              type: [
                "string",
                "null"
              ]
            },
            errorType: {
              description: "Open-ended classification; clients must accept categories added by Responses.",
              type: [
                "string",
                "null"
              ]
            },
            reviewTarget: {
              description: "Opaque server-issued block target. Presence alone does not enable target-based continuation.",
              type: [
                "string",
                "null"
              ]
            },
            steer: {
              anyOf: [
                {
                  $ref: "#/definitions/MisalignmentSteer"
                },
                {
                  type: "null"
                }
              ],
              description: "Instruction to submit as the next turn's user input if continuation is confirmed."
            }
          },
          type: "object"
        },
        MisalignmentSteer: {
          properties: {
            message: {
              type: "string"
            }
          },
          required: [
            "message"
          ],
          type: "object"
        },
        NonSteerableTurnKind: {
          enum: [
            "review",
            "compact"
          ],
          type: "string"
        },
        PatchApplyStatus: {
          enum: [
            "inProgress",
            "completed",
            "failed",
            "declined"
          ],
          type: "string"
        },
        PatchChangeKind: {
          oneOf: [
            {
              properties: {
                type: {
                  enum: [
                    "add"
                  ],
                  title: "AddPatchChangeKindType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "AddPatchChangeKind",
              type: "object"
            },
            {
              properties: {
                type: {
                  enum: [
                    "delete"
                  ],
                  title: "DeletePatchChangeKindType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "DeletePatchChangeKind",
              type: "object"
            },
            {
              properties: {
                move_path: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "update"
                  ],
                  title: "UpdatePatchChangeKindType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "UpdatePatchChangeKind",
              type: "object"
            }
          ]
        },
        ReasoningEffort: {
          description: "A non-empty reasoning effort value advertised by the model.",
          minLength: 1,
          type: "string"
        },
        SubAgentActivityKind: {
          enum: [
            "started",
            "interacted",
            "interrupted",
            "completed"
          ],
          type: "string"
        },
        TextElement: {
          properties: {
            byteRange: {
              allOf: [
                {
                  $ref: "#/definitions/ByteRange"
                }
              ],
              description: "Byte range in the parent `text` buffer that this element occupies."
            },
            placeholder: {
              description: "Optional human-readable placeholder for the element, displayed in the UI.",
              type: [
                "string",
                "null"
              ]
            }
          },
          required: [
            "byteRange"
          ],
          type: "object"
        },
        ThreadItem: {
          oneOf: [
            {
              properties: {
                clientId: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                content: {
                  items: {
                    $ref: "#/definitions/UserInput"
                  },
                  type: "array"
                },
                id: {
                  type: "string"
                },
                type: {
                  enum: [
                    "userMessage"
                  ],
                  title: "UserMessageThreadItemType",
                  type: "string"
                }
              },
              required: [
                "content",
                "id",
                "type"
              ],
              title: "UserMessageThreadItem",
              type: "object"
            },
            {
              properties: {
                fragments: {
                  items: {
                    $ref: "#/definitions/HookPromptFragment"
                  },
                  type: "array"
                },
                id: {
                  type: "string"
                },
                type: {
                  enum: [
                    "hookPrompt"
                  ],
                  title: "HookPromptThreadItemType",
                  type: "string"
                }
              },
              required: [
                "fragments",
                "id",
                "type"
              ],
              title: "HookPromptThreadItem",
              type: "object"
            },
            {
              properties: {
                delivery: {
                  anyOf: [
                    {
                      $ref: "#/definitions/AgentMessageDelivery"
                    },
                    {
                      type: "null"
                    }
                  ],
                  default: null
                },
                id: {
                  type: "string"
                },
                memoryCitation: {
                  anyOf: [
                    {
                      $ref: "#/definitions/MemoryCitation"
                    },
                    {
                      type: "null"
                    }
                  ],
                  default: null
                },
                phase: {
                  anyOf: [
                    {
                      $ref: "#/definitions/MessagePhase"
                    },
                    {
                      type: "null"
                    }
                  ],
                  default: null
                },
                questions: {
                  default: null,
                  items: {
                    $ref: "#/definitions/AsyncUserInputQuestion"
                  },
                  type: [
                    "array",
                    "null"
                  ]
                },
                text: {
                  type: "string"
                },
                type: {
                  enum: [
                    "agentMessage"
                  ],
                  title: "AgentMessageThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "text",
                "type"
              ],
              title: "AgentMessageThreadItem",
              type: "object"
            },
            {
              properties: {
                id: {
                  type: "string"
                },
                name: {
                  type: "string"
                },
                namespace: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                output: {
                  $ref: "#/definitions/FunctionCallOutputBody"
                },
                type: {
                  enum: [
                    "functionCallOutput"
                  ],
                  title: "FunctionCallOutputThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "name",
                "output",
                "type"
              ],
              title: "FunctionCallOutputThreadItem",
              type: "object"
            },
            {
              description: "EXPERIMENTAL - proposed plan item content. The completed plan item is authoritative and may not match the concatenation of `PlanDelta` text.",
              properties: {
                id: {
                  type: "string"
                },
                text: {
                  type: "string"
                },
                type: {
                  enum: [
                    "plan"
                  ],
                  title: "PlanThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "text",
                "type"
              ],
              title: "PlanThreadItem",
              type: "object"
            },
            {
              properties: {
                content: {
                  default: [],
                  items: {
                    type: "string"
                  },
                  type: "array"
                },
                id: {
                  type: "string"
                },
                summary: {
                  default: [],
                  items: {
                    type: "string"
                  },
                  type: "array"
                },
                type: {
                  enum: [
                    "reasoning"
                  ],
                  title: "ReasoningThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "type"
              ],
              title: "ReasoningThreadItem",
              type: "object"
            },
            {
              properties: {
                aggregatedOutput: {
                  description: "The command's output, aggregated from stdout and stderr.",
                  type: [
                    "string",
                    "null"
                  ]
                },
                command: {
                  description: "The command to be executed.",
                  type: "string"
                },
                commandActions: {
                  description: "A best-effort parsing of the command to understand the action(s) it will perform. This returns a list of CommandAction objects because a single shell command may be composed of many commands piped together.",
                  items: {
                    $ref: "#/definitions/CommandAction"
                  },
                  type: "array"
                },
                cwd: {
                  allOf: [
                    {
                      $ref: "#/definitions/LegacyAppPathString"
                    }
                  ],
                  description: "The command's working directory."
                },
                durationMs: {
                  description: "The duration of the command execution in milliseconds.",
                  format: "int64",
                  type: [
                    "integer",
                    "null"
                  ]
                },
                exitCode: {
                  description: "The command's exit code.",
                  format: "int32",
                  type: [
                    "integer",
                    "null"
                  ]
                },
                id: {
                  type: "string"
                },
                pluginId: {
                  default: null,
                  description: "Trusted first-party plugin id when this command resolves to one plugin script.",
                  type: [
                    "string",
                    "null"
                  ]
                },
                processId: {
                  description: "Identifier for the underlying PTY process (when available).",
                  type: [
                    "string",
                    "null"
                  ]
                },
                scriptPath: {
                  default: null,
                  description: "Safe plugin-relative path when this command resolves to one plugin script.",
                  type: [
                    "string",
                    "null"
                  ]
                },
                source: {
                  allOf: [
                    {
                      $ref: "#/definitions/CommandExecutionSource"
                    }
                  ],
                  default: "agent"
                },
                status: {
                  $ref: "#/definitions/CommandExecutionStatus"
                },
                type: {
                  enum: [
                    "commandExecution"
                  ],
                  title: "CommandExecutionThreadItemType",
                  type: "string"
                }
              },
              required: [
                "command",
                "commandActions",
                "cwd",
                "id",
                "status",
                "type"
              ],
              title: "CommandExecutionThreadItem",
              type: "object"
            },
            {
              properties: {
                changes: {
                  items: {
                    $ref: "#/definitions/FileUpdateChange"
                  },
                  type: "array"
                },
                id: {
                  type: "string"
                },
                status: {
                  $ref: "#/definitions/PatchApplyStatus"
                },
                type: {
                  enum: [
                    "fileChange"
                  ],
                  title: "FileChangeThreadItemType",
                  type: "string"
                }
              },
              required: [
                "changes",
                "id",
                "status",
                "type"
              ],
              title: "FileChangeThreadItem",
              type: "object"
            },
            {
              properties: {
                appContext: {
                  anyOf: [
                    {
                      $ref: "#/definitions/McpToolCallAppContext"
                    },
                    {
                      type: "null"
                    }
                  ]
                },
                arguments: true,
                durationMs: {
                  description: "The duration of the MCP tool call in milliseconds.",
                  format: "int64",
                  type: [
                    "integer",
                    "null"
                  ]
                },
                error: {
                  anyOf: [
                    {
                      $ref: "#/definitions/McpToolCallError"
                    },
                    {
                      type: "null"
                    }
                  ]
                },
                id: {
                  type: "string"
                },
                mcpAppResourceUri: {
                  description: "Legacy compatibility field; prefer `mcpAppUi.resourceUri` when available.",
                  type: [
                    "string",
                    "null"
                  ]
                },
                mcpAppUi: {
                  anyOf: [
                    {
                      $ref: "#/definitions/McpAppUi"
                    },
                    {
                      type: "null"
                    }
                  ],
                  description: "Presentation captured from the invoked descriptor; absent in older history."
                },
                pluginId: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                readOnlyHint: {
                  type: [
                    "boolean",
                    "null"
                  ]
                },
                result: {
                  anyOf: [
                    {
                      $ref: "#/definitions/McpToolCallResult"
                    },
                    {
                      type: "null"
                    }
                  ]
                },
                server: {
                  type: "string"
                },
                status: {
                  $ref: "#/definitions/McpToolCallStatus"
                },
                tool: {
                  type: "string"
                },
                type: {
                  enum: [
                    "mcpToolCall"
                  ],
                  title: "McpToolCallThreadItemType",
                  type: "string"
                }
              },
              required: [
                "arguments",
                "id",
                "server",
                "status",
                "tool",
                "type"
              ],
              title: "McpToolCallThreadItem",
              type: "object"
            },
            {
              properties: {
                arguments: true,
                contentItems: {
                  items: {
                    $ref: "#/definitions/DynamicToolCallOutputContentItem"
                  },
                  type: [
                    "array",
                    "null"
                  ]
                },
                durationMs: {
                  description: "The duration of the dynamic tool call in milliseconds.",
                  format: "int64",
                  type: [
                    "integer",
                    "null"
                  ]
                },
                id: {
                  type: "string"
                },
                namespace: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                status: {
                  $ref: "#/definitions/DynamicToolCallStatus"
                },
                success: {
                  type: [
                    "boolean",
                    "null"
                  ]
                },
                tool: {
                  type: "string"
                },
                type: {
                  enum: [
                    "dynamicToolCall"
                  ],
                  title: "DynamicToolCallThreadItemType",
                  type: "string"
                }
              },
              required: [
                "arguments",
                "id",
                "status",
                "tool",
                "type"
              ],
              title: "DynamicToolCallThreadItem",
              type: "object"
            },
            {
              properties: {
                agentsStates: {
                  additionalProperties: {
                    $ref: "#/definitions/CollabAgentState"
                  },
                  description: "Last known status of the target agents, when available.",
                  type: "object"
                },
                id: {
                  description: "Unique identifier for this collab tool call.",
                  type: "string"
                },
                model: {
                  description: "Model requested for the spawned agent, when applicable.",
                  type: [
                    "string",
                    "null"
                  ]
                },
                prompt: {
                  description: "Prompt text sent as part of the collab tool call, when available.",
                  type: [
                    "string",
                    "null"
                  ]
                },
                reasoningEffort: {
                  anyOf: [
                    {
                      $ref: "#/definitions/ReasoningEffort"
                    },
                    {
                      type: "null"
                    }
                  ],
                  description: "Reasoning effort requested for the spawned agent, when applicable."
                },
                receiverThreadIds: {
                  description: "Thread ID of the receiving agent, when applicable. In case of spawn operation, this corresponds to the newly spawned agent.",
                  items: {
                    type: "string"
                  },
                  type: "array"
                },
                senderThreadId: {
                  description: "Thread ID of the agent issuing the collab request.",
                  type: "string"
                },
                status: {
                  allOf: [
                    {
                      $ref: "#/definitions/CollabAgentToolCallStatus"
                    }
                  ],
                  description: "Current status of the collab tool call."
                },
                tool: {
                  allOf: [
                    {
                      $ref: "#/definitions/CollabAgentTool"
                    }
                  ],
                  description: "Name of the collab tool that was invoked."
                },
                type: {
                  enum: [
                    "collabAgentToolCall"
                  ],
                  title: "CollabAgentToolCallThreadItemType",
                  type: "string"
                }
              },
              required: [
                "agentsStates",
                "id",
                "receiverThreadIds",
                "senderThreadId",
                "status",
                "tool",
                "type"
              ],
              title: "CollabAgentToolCallThreadItem",
              type: "object"
            },
            {
              properties: {
                agentPath: {
                  type: "string"
                },
                agentThreadId: {
                  type: "string"
                },
                id: {
                  type: "string"
                },
                kind: {
                  $ref: "#/definitions/SubAgentActivityKind"
                },
                model: {
                  description: "Resolved model at sub-agent creation; absent from older records and other activities.",
                  type: [
                    "string",
                    "null"
                  ]
                },
                reasoningEffort: {
                  anyOf: [
                    {
                      $ref: "#/definitions/ReasoningEffort"
                    },
                    {
                      type: "null"
                    }
                  ],
                  description: "Resolved reasoning effort at sub-agent creation, when known."
                },
                type: {
                  enum: [
                    "subAgentActivity"
                  ],
                  title: "SubAgentActivityThreadItemType",
                  type: "string"
                }
              },
              required: [
                "agentPath",
                "agentThreadId",
                "id",
                "kind",
                "type"
              ],
              title: "SubAgentActivityThreadItem",
              type: "object"
            },
            {
              properties: {
                action: {
                  anyOf: [
                    {
                      $ref: "#/definitions/WebSearchAction"
                    },
                    {
                      type: "null"
                    }
                  ]
                },
                id: {
                  type: "string"
                },
                query: {
                  type: "string"
                },
                results: {
                  default: null,
                  description: "Structured search results returned out-of-band by standalone web search.\n\nThese stay as opaque JSON at the extension/app-server boundary so new result fields and result types can pass through without a Codex release.",
                  items: true,
                  type: [
                    "array",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "webSearch"
                  ],
                  title: "WebSearchThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "query",
                "type"
              ],
              title: "WebSearchThreadItem",
              type: "object"
            },
            {
              properties: {
                id: {
                  type: "string"
                },
                path: {
                  $ref: "#/definitions/LegacyAppPathString"
                },
                type: {
                  enum: [
                    "imageView"
                  ],
                  title: "ImageViewThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "path",
                "type"
              ],
              title: "ImageViewThreadItem",
              type: "object"
            },
            {
              description: "Display item emitted by the interruptible `clock.sleep` tool.",
              properties: {
                durationMs: {
                  format: "uint64",
                  minimum: 0,
                  type: "integer"
                },
                id: {
                  type: "string"
                },
                type: {
                  enum: [
                    "sleep"
                  ],
                  title: "SleepThreadItemType",
                  type: "string"
                }
              },
              required: [
                "durationMs",
                "id",
                "type"
              ],
              title: "SleepThreadItem",
              type: "object"
            },
            {
              properties: {
                failure: {
                  anyOf: [
                    {
                      $ref: "#/definitions/ImageGenerationFailure"
                    },
                    {
                      type: "null"
                    }
                  ],
                  default: null
                },
                id: {
                  type: "string"
                },
                result: {
                  type: "string"
                },
                revisedPrompt: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                savedPath: {
                  anyOf: [
                    {
                      $ref: "#/definitions/AbsolutePathBuf"
                    },
                    {
                      type: "null"
                    }
                  ]
                },
                status: {
                  type: "string"
                },
                transparentBackground: {
                  default: null,
                  type: [
                    "boolean",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "imageGeneration"
                  ],
                  title: "ImageGenerationThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "result",
                "status",
                "type"
              ],
              title: "ImageGenerationThreadItem",
              type: "object"
            },
            {
              properties: {
                id: {
                  type: "string"
                },
                review: {
                  type: "string"
                },
                type: {
                  enum: [
                    "enteredReviewMode"
                  ],
                  title: "EnteredReviewModeThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "review",
                "type"
              ],
              title: "EnteredReviewModeThreadItem",
              type: "object"
            },
            {
              properties: {
                id: {
                  type: "string"
                },
                review: {
                  type: "string"
                },
                type: {
                  enum: [
                    "exitedReviewMode"
                  ],
                  title: "ExitedReviewModeThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "review",
                "type"
              ],
              title: "ExitedReviewModeThreadItem",
              type: "object"
            },
            {
              properties: {
                id: {
                  type: "string"
                },
                type: {
                  enum: [
                    "contextCompaction"
                  ],
                  title: "ContextCompactionThreadItemType",
                  type: "string"
                }
              },
              required: [
                "id",
                "type"
              ],
              title: "ContextCompactionThreadItem",
              type: "object"
            }
          ]
        },
        Turn: {
          properties: {
            completedAt: {
              description: "Unix timestamp (in seconds) when the turn completed.",
              format: "int64",
              type: [
                "integer",
                "null"
              ]
            },
            durationMs: {
              description: "Duration between turn start and completion in milliseconds, if known.",
              format: "int64",
              type: [
                "integer",
                "null"
              ]
            },
            error: {
              anyOf: [
                {
                  $ref: "#/definitions/TurnError"
                },
                {
                  type: "null"
                }
              ],
              description: "Error associated with a failed or interrupted turn."
            },
            id: {
              description: "Identifier for this turn. Codex-generated turn IDs are UUIDv7.",
              type: "string"
            },
            items: {
              description: "Thread items currently included in this turn payload.",
              items: {
                $ref: "#/definitions/ThreadItem"
              },
              type: "array"
            },
            itemsView: {
              allOf: [
                {
                  $ref: "#/definitions/TurnItemsView"
                }
              ],
              default: "full",
              description: "Describes how much of `items` has been loaded for this turn."
            },
            rootTurnId: {
              description: "ID of the first turn in the chain of work that led to this turn. Pass this as `rootTurnId` when starting work on behalf of this turn. May be null in older history or a `review/start` response.",
              type: [
                "string",
                "null"
              ]
            },
            startedAt: {
              description: "Unix timestamp (in seconds) when the turn started.",
              format: "int64",
              type: [
                "integer",
                "null"
              ]
            },
            status: {
              $ref: "#/definitions/TurnStatus"
            }
          },
          required: [
            "id",
            "items",
            "status"
          ],
          type: "object"
        },
        TurnError: {
          properties: {
            additionalDetails: {
              default: null,
              type: [
                "string",
                "null"
              ]
            },
            codexErrorInfo: {
              anyOf: [
                {
                  $ref: "#/definitions/CodexErrorInfo"
                },
                {
                  type: "null"
                }
              ]
            },
            message: {
              type: "string"
            },
            misalignment: {
              anyOf: [
                {
                  $ref: "#/definitions/MisalignmentErrorDetails"
                },
                {
                  type: "null"
                }
              ],
              default: null,
              description: "Optional public explanation and continuation instruction for a misalignment block."
            }
          },
          required: [
            "message"
          ],
          type: "object"
        },
        TurnItemsView: {
          oneOf: [
            {
              description: "`items` was not loaded for this turn. The field is intentionally empty.",
              enum: [
                "notLoaded"
              ],
              type: "string"
            },
            {
              description: "`items` contains only a display summary for this turn.",
              enum: [
                "summary"
              ],
              type: "string"
            },
            {
              description: "`items` contains every ThreadItem available from persisted app-server history for this turn.",
              enum: [
                "full"
              ],
              type: "string"
            }
          ]
        },
        TurnStatus: {
          enum: [
            "completed",
            "interrupted",
            "failed",
            "inProgress"
          ],
          type: "string"
        },
        UserInput: {
          oneOf: [
            {
              properties: {
                text: {
                  type: "string"
                },
                text_elements: {
                  default: [],
                  description: "UI-defined spans within `text` used to render or persist special elements.",
                  items: {
                    $ref: "#/definitions/TextElement"
                  },
                  type: "array"
                },
                type: {
                  enum: [
                    "text"
                  ],
                  title: "TextUserInputType",
                  type: "string"
                }
              },
              required: [
                "text",
                "type"
              ],
              title: "TextUserInput",
              type: "object"
            },
            {
              anyOf: [
                {
                  properties: {
                    url: {
                      type: "string"
                    }
                  },
                  required: [
                    "url"
                  ],
                  title: "UrlUserInput",
                  type: "object"
                },
                {
                  properties: {
                    fileId: {
                      type: "string"
                    }
                  },
                  required: [
                    "fileId"
                  ],
                  title: "FileIdUserInput",
                  type: "object"
                }
              ],
              properties: {
                detail: {
                  anyOf: [
                    {
                      $ref: "#/definitions/ImageDetail"
                    },
                    {
                      type: "null"
                    }
                  ],
                  default: null
                },
                type: {
                  enum: [
                    "image"
                  ],
                  title: "ImageUserInputType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "ImageUserInput",
              type: "object"
            },
            {
              properties: {
                detail: {
                  anyOf: [
                    {
                      $ref: "#/definitions/ImageDetail"
                    },
                    {
                      type: "null"
                    }
                  ],
                  default: null
                },
                path: {
                  type: "string"
                },
                type: {
                  enum: [
                    "localImage"
                  ],
                  title: "LocalImageUserInputType",
                  type: "string"
                }
              },
              required: [
                "path",
                "type"
              ],
              title: "LocalImageUserInput",
              type: "object"
            },
            {
              properties: {
                type: {
                  enum: [
                    "audio"
                  ],
                  title: "AudioUserInputType",
                  type: "string"
                },
                url: {
                  type: "string"
                }
              },
              required: [
                "type",
                "url"
              ],
              title: "AudioUserInput",
              type: "object"
            },
            {
              properties: {
                path: {
                  type: "string"
                },
                type: {
                  enum: [
                    "localAudio"
                  ],
                  title: "LocalAudioUserInputType",
                  type: "string"
                }
              },
              required: [
                "path",
                "type"
              ],
              title: "LocalAudioUserInput",
              type: "object"
            },
            {
              properties: {
                name: {
                  type: "string"
                },
                path: {
                  type: "string"
                },
                type: {
                  enum: [
                    "skill"
                  ],
                  title: "SkillUserInputType",
                  type: "string"
                }
              },
              required: [
                "name",
                "path",
                "type"
              ],
              title: "SkillUserInput",
              type: "object"
            },
            {
              properties: {
                name: {
                  type: "string"
                },
                path: {
                  type: "string"
                },
                type: {
                  enum: [
                    "mention"
                  ],
                  title: "MentionUserInputType",
                  type: "string"
                }
              },
              required: [
                "name",
                "path",
                "type"
              ],
              title: "MentionUserInput",
              type: "object"
            }
          ]
        },
        WebSearchAction: {
          oneOf: [
            {
              properties: {
                queries: {
                  items: {
                    type: "string"
                  },
                  type: [
                    "array",
                    "null"
                  ]
                },
                query: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "search"
                  ],
                  title: "SearchWebSearchActionType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "SearchWebSearchAction",
              type: "object"
            },
            {
              properties: {
                type: {
                  enum: [
                    "openPage"
                  ],
                  title: "OpenPageWebSearchActionType",
                  type: "string"
                },
                url: {
                  type: [
                    "string",
                    "null"
                  ]
                }
              },
              required: [
                "type"
              ],
              title: "OpenPageWebSearchAction",
              type: "object"
            },
            {
              properties: {
                pattern: {
                  type: [
                    "string",
                    "null"
                  ]
                },
                type: {
                  enum: [
                    "findInPage"
                  ],
                  title: "FindInPageWebSearchActionType",
                  type: "string"
                },
                url: {
                  type: [
                    "string",
                    "null"
                  ]
                }
              },
              required: [
                "type"
              ],
              title: "FindInPageWebSearchAction",
              type: "object"
            },
            {
              properties: {
                type: {
                  enum: [
                    "other"
                  ],
                  title: "OtherWebSearchActionType",
                  type: "string"
                }
              },
              required: [
                "type"
              ],
              title: "OtherWebSearchAction",
              type: "object"
            }
          ]
        }
      },
      properties: {
        turn: {
          $ref: "#/definitions/Turn"
        }
      },
      required: [
        "turn"
      ],
      title: "TurnStartResponse",
      type: "object"
    };
  }
});

// src/codex-protocol/schema/v2/TurnInterruptResponse.json
var TurnInterruptResponse_default;
var init_TurnInterruptResponse = __esm({
  "src/codex-protocol/schema/v2/TurnInterruptResponse.json"() {
    TurnInterruptResponse_default = {
      $schema: "http://json-schema.org/draft-07/schema#",
      title: "TurnInterruptResponse",
      type: "object"
    };
  }
});

// src/codex-protocol/schema/PermissionsRequestApprovalResponse.json
var PermissionsRequestApprovalResponse_default;
var init_PermissionsRequestApprovalResponse = __esm({
  "src/codex-protocol/schema/PermissionsRequestApprovalResponse.json"() {
    PermissionsRequestApprovalResponse_default = {
      $schema: "http://json-schema.org/draft-07/schema#",
      definitions: {
        AdditionalFileSystemPermissions: {
          properties: {
            entries: {
              items: {
                $ref: "#/definitions/FileSystemSandboxEntry"
              },
              type: [
                "array",
                "null"
              ]
            },
            globScanMaxDepth: {
              format: "uint",
              minimum: 1,
              type: [
                "integer",
                "null"
              ]
            },
            read: {
              description: "This will be removed in favor of `entries`.",
              items: {
                $ref: "#/definitions/LegacyAppPathString"
              },
              type: [
                "array",
                "null"
              ]
            },
            write: {
              description: "This will be removed in favor of `entries`.",
              items: {
                $ref: "#/definitions/LegacyAppPathString"
              },
              type: [
                "array",
                "null"
              ]
            }
          },
          type: "object"
        },
        AdditionalNetworkPermissions: {
          properties: {
            enabled: {
              type: [
                "boolean",
                "null"
              ]
            }
          },
          type: "object"
        },
        FileSystemAccessMode: {
          enum: [
            "read",
            "write",
            "deny"
          ],
          type: "string"
        },
        FileSystemPath: {
          oneOf: [
            {
              properties: {
                path: {
                  $ref: "#/definitions/LegacyAppPathString"
                },
                type: {
                  enum: [
                    "path"
                  ],
                  title: "PathFileSystemPathType",
                  type: "string"
                }
              },
              required: [
                "path",
                "type"
              ],
              title: "PathFileSystemPath",
              type: "object"
            },
            {
              properties: {
                pattern: {
                  type: "string"
                },
                type: {
                  enum: [
                    "glob_pattern"
                  ],
                  title: "GlobPatternFileSystemPathType",
                  type: "string"
                }
              },
              required: [
                "pattern",
                "type"
              ],
              title: "GlobPatternFileSystemPath",
              type: "object"
            },
            {
              properties: {
                type: {
                  enum: [
                    "special"
                  ],
                  title: "SpecialFileSystemPathType",
                  type: "string"
                },
                value: {
                  $ref: "#/definitions/FileSystemSpecialPath"
                }
              },
              required: [
                "type",
                "value"
              ],
              title: "SpecialFileSystemPath",
              type: "object"
            }
          ]
        },
        FileSystemSandboxEntry: {
          properties: {
            access: {
              $ref: "#/definitions/FileSystemAccessMode"
            },
            path: {
              $ref: "#/definitions/FileSystemPath"
            }
          },
          required: [
            "access",
            "path"
          ],
          type: "object"
        },
        FileSystemSpecialPath: {
          oneOf: [
            {
              properties: {
                kind: {
                  enum: [
                    "root"
                  ],
                  type: "string"
                }
              },
              required: [
                "kind"
              ],
              title: "RootFileSystemSpecialPath",
              type: "object"
            },
            {
              properties: {
                kind: {
                  enum: [
                    "minimal"
                  ],
                  type: "string"
                }
              },
              required: [
                "kind"
              ],
              title: "MinimalFileSystemSpecialPath",
              type: "object"
            },
            {
              properties: {
                kind: {
                  enum: [
                    "project_roots"
                  ],
                  type: "string"
                },
                subpath: {
                  anyOf: [
                    {
                      $ref: "#/definitions/LegacyAppPathString"
                    },
                    {
                      type: "null"
                    }
                  ]
                }
              },
              required: [
                "kind"
              ],
              title: "KindFileSystemSpecialPath",
              type: "object"
            },
            {
              properties: {
                kind: {
                  enum: [
                    "tmpdir"
                  ],
                  type: "string"
                }
              },
              required: [
                "kind"
              ],
              title: "TmpdirFileSystemSpecialPath",
              type: "object"
            },
            {
              properties: {
                kind: {
                  enum: [
                    "slash_tmp"
                  ],
                  type: "string"
                }
              },
              required: [
                "kind"
              ],
              title: "SlashTmpFileSystemSpecialPath",
              type: "object"
            },
            {
              properties: {
                kind: {
                  enum: [
                    "unknown"
                  ],
                  type: "string"
                },
                path: {
                  type: "string"
                },
                subpath: {
                  anyOf: [
                    {
                      $ref: "#/definitions/LegacyAppPathString"
                    },
                    {
                      type: "null"
                    }
                  ]
                }
              },
              required: [
                "kind",
                "path"
              ],
              type: "object"
            }
          ]
        },
        GrantedPermissionProfile: {
          properties: {
            fileSystem: {
              anyOf: [
                {
                  $ref: "#/definitions/AdditionalFileSystemPermissions"
                },
                {
                  type: "null"
                }
              ]
            },
            network: {
              anyOf: [
                {
                  $ref: "#/definitions/AdditionalNetworkPermissions"
                },
                {
                  type: "null"
                }
              ]
            }
          },
          type: "object"
        },
        LegacyAppPathString: {
          type: "string"
        },
        PermissionGrantScope: {
          enum: [
            "turn",
            "session"
          ],
          type: "string"
        }
      },
      properties: {
        permissions: {
          $ref: "#/definitions/GrantedPermissionProfile"
        },
        scope: {
          allOf: [
            {
              $ref: "#/definitions/PermissionGrantScope"
            }
          ],
          default: "turn"
        },
        strictAutoReview: {
          description: "Review every subsequent command in this turn before normal sandboxed execution.",
          type: [
            "boolean",
            "null"
          ]
        }
      },
      required: [
        "permissions"
      ],
      title: "PermissionsRequestApprovalResponse",
      type: "object"
    };
  }
});

// src/codex-protocol/schema/McpServerElicitationRequestResponse.json
var McpServerElicitationRequestResponse_default;
var init_McpServerElicitationRequestResponse = __esm({
  "src/codex-protocol/schema/McpServerElicitationRequestResponse.json"() {
    McpServerElicitationRequestResponse_default = {
      $schema: "http://json-schema.org/draft-07/schema#",
      definitions: {
        McpServerElicitationAction: {
          enum: [
            "accept",
            "decline",
            "cancel"
          ],
          type: "string"
        }
      },
      properties: {
        _meta: {
          description: "Optional client metadata for form-mode action handling."
        },
        action: {
          $ref: "#/definitions/McpServerElicitationAction"
        },
        content: {
          description: "Structured user input for accepted elicitations, mirroring RMCP `CreateElicitationResult`.\n\nThis is nullable because decline/cancel responses have no content."
        }
      },
      required: [
        "action"
      ],
      title: "McpServerElicitationRequestResponse",
      type: "object"
    };
  }
});

// src/codex-protocol/source.json
var source_default;
var init_source = __esm({
  "src/codex-protocol/source.json"() {
    source_default = {
      version: "0.162.1",
      repository: "https://github.com/openai/codex",
      tag: "rust-v0.162.1",
      commit: "092d3acd6bec3e3a14bdc7e7a2810ab628ab759d",
      typescriptRoots: [
        "InitializeParams.ts",
        "InitializeResponse.ts",
        "ServerRequest.ts",
        "ServerNotification.ts",
        "RequestId.ts",
        "v2/ThreadStartParams.ts",
        "v2/ThreadStartResponse.ts",
        "v2/TurnStartParams.ts",
        "v2/TurnStartResponse.ts",
        "v2/TurnInterruptParams.ts",
        "v2/TurnInterruptResponse.ts",
        "v2/CommandExecutionRequestApprovalResponse.ts",
        "v2/FileChangeRequestApprovalResponse.ts",
        "v2/PermissionsRequestApprovalResponse.ts",
        "v2/ToolRequestUserInputResponse.ts",
        "v2/McpServerElicitationRequestResponse.ts",
        "v2/ThreadResumeParams.ts",
        "v2/ThreadResumeResponse.ts"
      ],
      schemas: [
        "ServerRequest.json",
        "ServerNotification.json",
        "v1/InitializeResponse.json",
        "v2/ThreadStartResponse.json",
        "v2/TurnStartResponse.json",
        "v2/TurnInterruptResponse.json",
        "PermissionsRequestApprovalResponse.json",
        "McpServerElicitationRequestResponse.json",
        "v2/ThreadResumeResponse.json"
      ],
      files: [
        "AbsolutePathBuf.ts",
        "AgentMessageInputContent.ts",
        "AgentPath.ts",
        "ApplyPatchApprovalParams.ts",
        "AuthMode.ts",
        "ClientInfo.ts",
        "CollaborationMode.ts",
        "ConfigurationReasoning.ts",
        "ContentItem.ts",
        "ExecCommandApprovalParams.ts",
        "FileChange.ts",
        "FunctionCallOutputBody.ts",
        "FunctionCallOutputContentItem.ts",
        "FuzzyFileSearchMatchType.ts",
        "FuzzyFileSearchResult.ts",
        "FuzzyFileSearchSessionCompletedNotification.ts",
        "FuzzyFileSearchSessionUpdatedNotification.ts",
        "ImageDetail.ts",
        "ImageGenerationFailure.ts",
        "ImageGenerationItem.ts",
        "InitializeCapabilities.ts",
        "InitializeParams.ts",
        "InitializeResponse.ts",
        "InternalChatMessageMetadataPassthrough.ts",
        "LegacyAppPathString.ts",
        "LocalShellAction.ts",
        "LocalShellExecAction.ts",
        "LocalShellStatus.ts",
        "MessagePhase.ts",
        "ModeKind.ts",
        "ParsedCommand.ts",
        "Personality.ts",
        "PlanType.ts",
        "RealtimeConversationVersion.ts",
        "ReasoningEffort.ts",
        "ReasoningItemContent.ts",
        "ReasoningItemReasoningSummary.ts",
        "ReasoningSummary.ts",
        "RequestId.ts",
        "ResponseItem.ts",
        "ResponseItemId.ts",
        "ServerNotification.ts",
        "ServerRequest.ts",
        "Settings.ts",
        "SleepItem.ts",
        "SubAgentSource.ts",
        "ThreadId.ts",
        "WebSearchAction.ts",
        "WebSearchItem.ts",
        "serde_json/JsonValue.ts",
        "v2/AccountLoginCompletedNotification.ts",
        "v2/AccountRateLimitsUpdatedNotification.ts",
        "v2/AccountUpdatedNotification.ts",
        "v2/ActivePermissionProfile.ts",
        "v2/AdditionalFileSystemPermissions.ts",
        "v2/AdditionalNetworkPermissions.ts",
        "v2/AgentMessageDelivery.ts",
        "v2/AgentMessageDeltaNotification.ts",
        "v2/AppBranding.ts",
        "v2/AppInfo.ts",
        "v2/AppListUpdatedNotification.ts",
        "v2/AppMetadata.ts",
        "v2/AppReview.ts",
        "v2/AppScreenshot.ts",
        "v2/ApprovalsReviewer.ts",
        "v2/AskForApproval.ts",
        "v2/AsyncUserInputQuestion.ts",
        "v2/AttestationGenerateParams.ts",
        "v2/AuthRecoveryNotification.ts",
        "v2/AutoReviewDecisionSource.ts",
        "v2/ByteRange.ts",
        "v2/ChatgptAuthTokensRefreshParams.ts",
        "v2/ChatgptAuthTokensRefreshReason.ts",
        "v2/CodexErrorInfo.ts",
        "v2/CollabAgentState.ts",
        "v2/CollabAgentStatus.ts",
        "v2/CollabAgentTool.ts",
        "v2/CollabAgentToolCallStatus.ts",
        "v2/CommandAction.ts",
        "v2/CommandExecOutputDeltaNotification.ts",
        "v2/CommandExecOutputStream.ts",
        "v2/CommandExecutionApprovalDecision.ts",
        "v2/CommandExecutionApprovalKind.ts",
        "v2/CommandExecutionOutputDeltaNotification.ts",
        "v2/CommandExecutionRequestApprovalParams.ts",
        "v2/CommandExecutionRequestApprovalResponse.ts",
        "v2/CommandExecutionSource.ts",
        "v2/CommandExecutionStatus.ts",
        "v2/ConfigWarningNotification.ts",
        "v2/ContextCompactedNotification.ts",
        "v2/CreditsSnapshot.ts",
        "v2/DeprecationNoticeNotification.ts",
        "v2/DesktopOnboardingEntrypoint.ts",
        "v2/DynamicToolCallOutputContentItem.ts",
        "v2/DynamicToolCallParams.ts",
        "v2/DynamicToolCallStatus.ts",
        "v2/EnvironmentConnectionNotification.ts",
        "v2/ErrorNotification.ts",
        "v2/ExecPolicyAmendment.ts",
        "v2/ExternalAgentConfigImportCompletedNotification.ts",
        "v2/ExternalAgentConfigImportItemTypeFailure.ts",
        "v2/ExternalAgentConfigImportItemTypeSuccess.ts",
        "v2/ExternalAgentConfigImportProgressNotification.ts",
        "v2/ExternalAgentConfigImportTypeResult.ts",
        "v2/ExternalAgentConfigMigrationItemType.ts",
        "v2/FileChangeApprovalDecision.ts",
        "v2/FileChangeOutputDeltaNotification.ts",
        "v2/FileChangePatchUpdatedNotification.ts",
        "v2/FileChangeRequestApprovalParams.ts",
        "v2/FileChangeRequestApprovalResponse.ts",
        "v2/FileSystemAccessMode.ts",
        "v2/FileSystemPath.ts",
        "v2/FileSystemSandboxEntry.ts",
        "v2/FileSystemSpecialPath.ts",
        "v2/FileUpdateChange.ts",
        "v2/FsChangedNotification.ts",
        "v2/GatewayOAuthChangedNotification.ts",
        "v2/GatewayOAuthStatus.ts",
        "v2/GitInfo.ts",
        "v2/GrantedPermissionProfile.ts",
        "v2/GuardianApprovalReview.ts",
        "v2/GuardianApprovalReviewAction.ts",
        "v2/GuardianApprovalReviewStatus.ts",
        "v2/GuardianCommandSource.ts",
        "v2/GuardianRiskLevel.ts",
        "v2/GuardianUserAuthorization.ts",
        "v2/GuardianWarningNotification.ts",
        "v2/HookCompletedNotification.ts",
        "v2/HookEventName.ts",
        "v2/HookExecutionMode.ts",
        "v2/HookHandlerType.ts",
        "v2/HookOutputEntry.ts",
        "v2/HookOutputEntryKind.ts",
        "v2/HookPromptFragment.ts",
        "v2/HookRunStatus.ts",
        "v2/HookRunSummary.ts",
        "v2/HookScope.ts",
        "v2/HookSource.ts",
        "v2/HookStartedNotification.ts",
        "v2/ItemCompletedNotification.ts",
        "v2/ItemGuardianApprovalReviewCompletedNotification.ts",
        "v2/ItemGuardianApprovalReviewStartedNotification.ts",
        "v2/ItemStartedNotification.ts",
        "v2/McpAppDisplayMode.ts",
        "v2/McpAppUi.ts",
        "v2/McpElicitationArrayType.ts",
        "v2/McpElicitationBooleanSchema.ts",
        "v2/McpElicitationBooleanType.ts",
        "v2/McpElicitationConstOption.ts",
        "v2/McpElicitationEnumSchema.ts",
        "v2/McpElicitationLegacyTitledEnumSchema.ts",
        "v2/McpElicitationMultiSelectEnumSchema.ts",
        "v2/McpElicitationNumberSchema.ts",
        "v2/McpElicitationNumberType.ts",
        "v2/McpElicitationObjectType.ts",
        "v2/McpElicitationPrimitiveSchema.ts",
        "v2/McpElicitationSchema.ts",
        "v2/McpElicitationSingleSelectEnumSchema.ts",
        "v2/McpElicitationStringFormat.ts",
        "v2/McpElicitationStringSchema.ts",
        "v2/McpElicitationStringType.ts",
        "v2/McpElicitationTitledEnumItems.ts",
        "v2/McpElicitationTitledMultiSelectEnumSchema.ts",
        "v2/McpElicitationTitledSingleSelectEnumSchema.ts",
        "v2/McpElicitationUntitledEnumItems.ts",
        "v2/McpElicitationUntitledMultiSelectEnumSchema.ts",
        "v2/McpElicitationUntitledSingleSelectEnumSchema.ts",
        "v2/McpServerElicitationAction.ts",
        "v2/McpServerElicitationRequestParams.ts",
        "v2/McpServerElicitationRequestResponse.ts",
        "v2/McpServerEventNotification.ts",
        "v2/McpServerEventStreamNotification.ts",
        "v2/McpServerOauthLoginCompletedNotification.ts",
        "v2/McpServerStartupFailureReason.ts",
        "v2/McpServerStartupState.ts",
        "v2/McpServerStatusUpdatedNotification.ts",
        "v2/McpToolCallAppContext.ts",
        "v2/McpToolCallError.ts",
        "v2/McpToolCallProgressNotification.ts",
        "v2/McpToolCallResult.ts",
        "v2/McpToolCallStatus.ts",
        "v2/MemoryCitation.ts",
        "v2/MemoryCitationEntry.ts",
        "v2/MisalignmentErrorDetails.ts",
        "v2/MisalignmentSteer.ts",
        "v2/ModelRerouteReason.ts",
        "v2/ModelReroutedNotification.ts",
        "v2/ModelSafetyBufferingUpdatedNotification.ts",
        "v2/ModelVerification.ts",
        "v2/ModelVerificationNotification.ts",
        "v2/NetworkAccess.ts",
        "v2/NetworkApprovalContext.ts",
        "v2/NetworkApprovalProtocol.ts",
        "v2/NetworkPolicyAmendment.ts",
        "v2/NetworkPolicyRuleAction.ts",
        "v2/NonSteerableTurnKind.ts",
        "v2/PatchApplyStatus.ts",
        "v2/PatchChangeKind.ts",
        "v2/PermissionGrantScope.ts",
        "v2/PermissionsRequestApprovalParams.ts",
        "v2/PermissionsRequestApprovalResponse.ts",
        "v2/PlanDeltaNotification.ts",
        "v2/ProcessExitedNotification.ts",
        "v2/ProcessOutputDeltaNotification.ts",
        "v2/ProcessOutputStream.ts",
        "v2/ProjectChangeType.ts",
        "v2/ProjectChangedNotification.ts",
        "v2/RateLimitReachedType.ts",
        "v2/RateLimitSnapshot.ts",
        "v2/RateLimitWindow.ts",
        "v2/RawResponseCompletedNotification.ts",
        "v2/RawResponseItemCompletedNotification.ts",
        "v2/ReasoningSummaryPartAddedNotification.ts",
        "v2/ReasoningSummaryTextDeltaNotification.ts",
        "v2/ReasoningTextDeltaNotification.ts",
        "v2/RemoteControlConnectionStatus.ts",
        "v2/RemoteControlStatusChangedNotification.ts",
        "v2/RequestPermissionProfile.ts",
        "v2/ResponseUsageMetadata.ts",
        "v2/SandboxMode.ts",
        "v2/SandboxPolicy.ts",
        "v2/ServerRequestResolvedNotification.ts",
        "v2/SessionSource.ts",
        "v2/SkillsChangedNotification.ts",
        "v2/SpendControlLimitSnapshot.ts",
        "v2/StrictReviewRequiredNotification.ts",
        "v2/SubAgentActivityKind.ts",
        "v2/TerminalInteractionNotification.ts",
        "v2/TextElement.ts",
        "v2/TextPosition.ts",
        "v2/TextRange.ts",
        "v2/Thread.ts",
        "v2/ThreadActiveFlag.ts",
        "v2/ThreadArchivedNotification.ts",
        "v2/ThreadAttachmentOperation.ts",
        "v2/ThreadAttachmentUpdatedNotification.ts",
        "v2/ThreadClosedNotification.ts",
        "v2/ThreadDeletedNotification.ts",
        "v2/ThreadGoal.ts",
        "v2/ThreadGoalClearedNotification.ts",
        "v2/ThreadGoalStatus.ts",
        "v2/ThreadGoalUpdatedNotification.ts",
        "v2/ThreadHistoryMode.ts",
        "v2/ThreadItem.ts",
        "v2/ThreadNameUpdatedNotification.ts",
        "v2/ThreadPredictionResult.ts",
        "v2/ThreadPredictionUpdatedNotification.ts",
        "v2/ThreadProjectUpdatedNotification.ts",
        "v2/ThreadQueueChangedNotification.ts",
        "v2/ThreadRealtimeAudioChunk.ts",
        "v2/ThreadRealtimeBemItemPresentation.ts",
        "v2/ThreadRealtimeClosedNotification.ts",
        "v2/ThreadRealtimeErrorNotification.ts",
        "v2/ThreadRealtimeItem.ts",
        "v2/ThreadRealtimeItemAddedNotification.ts",
        "v2/ThreadRealtimeItemCompletedNotification.ts",
        "v2/ThreadRealtimeItemStartedNotification.ts",
        "v2/ThreadRealtimeItemTranscriptDeltaNotification.ts",
        "v2/ThreadRealtimeOutputAudioDeltaNotification.ts",
        "v2/ThreadRealtimeSdpNotification.ts",
        "v2/ThreadRealtimeSessionOutcome.ts",
        "v2/ThreadRealtimeStartedNotification.ts",
        "v2/ThreadRealtimeTranscriptDeltaNotification.ts",
        "v2/ThreadRealtimeTranscriptDoneNotification.ts",
        "v2/ThreadRealtimeTranscriptRole.ts",
        "v2/ThreadResumeParams.ts",
        "v2/ThreadResumeResponse.ts",
        "v2/ThreadRevertedNotification.ts",
        "v2/ThreadSection.ts",
        "v2/ThreadSectionAppearance.ts",
        "v2/ThreadSettings.ts",
        "v2/ThreadSettingsUpdatedNotification.ts",
        "v2/ThreadSource.ts",
        "v2/ThreadStartParams.ts",
        "v2/ThreadStartResponse.ts",
        "v2/ThreadStartSource.ts",
        "v2/ThreadStartedNotification.ts",
        "v2/ThreadStatus.ts",
        "v2/ThreadStatusChangedNotification.ts",
        "v2/ThreadTokenUsage.ts",
        "v2/ThreadTokenUsageUpdatedNotification.ts",
        "v2/ThreadUnarchivedNotification.ts",
        "v2/TokenUsageBreakdown.ts",
        "v2/ToolRequestUserInputAnswer.ts",
        "v2/ToolRequestUserInputOption.ts",
        "v2/ToolRequestUserInputParams.ts",
        "v2/ToolRequestUserInputQuestion.ts",
        "v2/ToolRequestUserInputResponse.ts",
        "v2/Turn.ts",
        "v2/TurnCompletedNotification.ts",
        "v2/TurnDiffUpdatedNotification.ts",
        "v2/TurnError.ts",
        "v2/TurnInterruptParams.ts",
        "v2/TurnInterruptResponse.ts",
        "v2/TurnItemsView.ts",
        "v2/TurnModerationMetadataNotification.ts",
        "v2/TurnPlanStep.ts",
        "v2/TurnPlanStepStatus.ts",
        "v2/TurnPlanUpdatedNotification.ts",
        "v2/TurnStartParams.ts",
        "v2/TurnStartResponse.ts",
        "v2/TurnStartedNotification.ts",
        "v2/TurnStatus.ts",
        "v2/TurnToolOutput.ts",
        "v2/UserInput.ts",
        "v2/WarningNotification.ts",
        "v2/WebSearchAction.ts",
        "v2/WindowsSandboxSetupCompletedNotification.ts",
        "v2/WindowsSandboxSetupMode.ts",
        "v2/WindowsWorldWritableWarningNotification.ts"
      ]
    };
  }
});

// src/codex.ts
var codex_exports = {};
__export(codex_exports, {
  CODEX_VERSION: () => CODEX_VERSION,
  createAdapter: () => createAdapter4,
  questionId: () => questionId
});
import { execFile, spawn as spawn3 } from "node:child_process";
import { isAbsolute as isAbsolute2 } from "node:path";
import { StringDecoder } from "node:string_decoder";
import { promisify } from "node:util";
function object2(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
function json(value) {
  return JSON.parse(JSON.stringify(value));
}
function requestId(value) {
  return typeof value === "string" || typeof value === "number" && Number.isSafeInteger(value);
}
function questionId(id) {
  return JSON.stringify(id);
}
async function createAdapter4(context) {
  const [executable, ...args] = context.command;
  if (!executable || !isAbsolute2(executable)) throw new Error("Codex requires an explicit absolute executable path");
  const version = await execute(executable, [...args, "--version"], { cwd: context.cwd, env: context.env, timeout: 1e4, maxBuffer: 4096 });
  if (version.stdout.trim() !== `codex-cli ${CODEX_VERSION}`) {
    throw new Error(`Codex app-server requires executable version ${CODEX_VERSION}; regenerate and review the official protocol before changing the pin`);
  }
  const { Ajv: AjvConstructor } = await import("ajv");
  const { default: addFormats } = await import("ajv-formats");
  const ajv = new AjvConstructor({ strict: false, allErrors: false, validateFormats: true });
  addFormats(ajv);
  for (const name of ["uint", "uint64"]) ajv.addFormat(name, { type: "number", validate: (n) => Number.isSafeInteger(n) && n >= 0 });
  ajv.addFormat("uint16", { type: "number", validate: (n) => Number.isInteger(n) && n >= 0 && n <= 65535 });
  ajv.addFormat("uint32", { type: "number", validate: (n) => Number.isInteger(n) && n >= 0 && n <= 4294967295 });
  ajv.addFormat("int32", { type: "number", validate: (n) => Number.isInteger(n) && n >= -2147483648 && n <= 2147483647 });
  ajv.addFormat("int64", { type: "number", validate: (n) => Number.isSafeInteger(n) });
  ajv.addFormat("double", { type: "number", validate: Number.isFinite });
  const validators = {
    request: ajv.compile(ServerRequest_default),
    notification: ajv.compile(ServerNotification_default),
    initialize: ajv.compile(InitializeResponse_default),
    thread: ajv.compile(ThreadStartResponse_default),
    resume: ajv.compile(ThreadResumeResponse_default),
    turn: ajv.compile(TurnStartResponse_default),
    interrupt: ajv.compile(TurnInterruptResponse_default),
    permissions: ajv.compile(PermissionsRequestApprovalResponse_default),
    elicitation: ajv.compile(McpServerElicitationRequestResponse_default)
  };
  const child = spawn3(executable, [...args, "app-server", "--listen", "stdio://"], { cwd: context.cwd, env: context.env, stdio: ["pipe", "pipe", "pipe"], detached: false });
  const adapter = new CodexAdapter(context, child, ajv, validators);
  try {
    await adapter.initialize();
    return adapter;
  } catch (error) {
    await adapter.close();
    throw error;
  }
}
var CODEX_VERSION, MAX_FRAME_BYTES, MAX_QUESTION_BYTES, MAX_PENDING2, MAX_STREAM_BYTES, execute, RpcRejected, QuestionResolved, CodexRpc, CodexAdapter;
var init_codex = __esm({
  "src/codex.ts"() {
    "use strict";
    init_transcript();
    init_ServerRequest();
    init_ServerNotification();
    init_InitializeResponse();
    init_ThreadStartResponse();
    init_ThreadResumeResponse();
    init_TurnStartResponse();
    init_TurnInterruptResponse();
    init_PermissionsRequestApprovalResponse();
    init_McpServerElicitationRequestResponse();
    init_source();
    CODEX_VERSION = source_default.version;
    MAX_FRAME_BYTES = 4 * 1024 * 1024;
    MAX_QUESTION_BYTES = 12 * 1024;
    MAX_PENDING2 = 128;
    MAX_STREAM_BYTES = 12 * 1024;
    execute = promisify(execFile);
    RpcRejected = class extends Error {
    };
    QuestionResolved = class extends Error {
    };
    CodexRpc = class {
      constructor(child, receive, disconnected) {
        this.child = child;
        this.receive = receive;
        this.disconnected = disconnected;
        const exit = Promise.withResolvers();
        this.exited = exit.promise;
        child.once("close", () => exit.resolve());
        child.stdout.on("data", (chunk) => this.drain(chunk));
        child.stderr.resume();
        child.stdin.on("error", () => this.fail(new Error("Codex app-server input closed")));
        child.stdout.on("error", () => this.fail(new Error("Codex app-server output failed")));
        child.once("error", () => this.fail(new Error("Unable to launch Codex app-server")));
        child.once("close", () => this.fail(new Error("Codex app-server exited")));
        child.stdout.once("end", () => this.fail(new Error("Codex app-server output closed")));
      }
      child;
      receive;
      disconnected;
      pending = /* @__PURE__ */ new Map();
      decoder = new StringDecoder("utf8");
      buffer = "";
      nextId = 0;
      failure;
      ending = false;
      exited;
      drain(chunk) {
        if (this.failure) return;
        this.buffer += this.decoder.write(chunk);
        let newline;
        while ((newline = this.buffer.indexOf("\n")) >= 0) {
          const line = this.buffer.slice(0, newline);
          this.buffer = this.buffer.slice(newline + 1);
          if (Buffer.byteLength(line) > MAX_FRAME_BYTES) {
            this.fail(new Error("Codex protocol frame exceeds its bound"));
            return;
          }
          if (!line.trim()) continue;
          try {
            const packet = JSON.parse(line);
            if (!object2(packet)) throw new Error("Invalid Codex protocol envelope");
            if (packet.jsonrpc !== void 0 && packet.jsonrpc !== "2.0") throw new Error("Invalid JSON-RPC version");
            if (typeof packet.method === "string") this.receive(packet);
            else if (requestId(packet.id)) {
              const pending = this.pending.get(packet.id);
              if (!pending) continue;
              this.pending.delete(packet.id);
              clearTimeout(pending.timer);
              if (object2(packet.error) && typeof packet.error.code === "number") {
                pending.reject(new RpcRejected(`Codex rejected request (code ${packet.error.code})`));
              } else if ("result" in packet) pending.resolve(packet.result);
              else pending.reject(new Error("Invalid Codex response envelope"));
            } else throw new Error("Invalid Codex protocol envelope");
          } catch {
            this.fail(new Error("Invalid Codex app-server protocol frame"));
            return;
          }
        }
        if (Buffer.byteLength(this.buffer) > MAX_FRAME_BYTES) this.fail(new Error("Codex protocol frame exceeds its bound"));
      }
      send(packet) {
        if (this.failure) throw this.failure;
        if (this.ending) throw new Error("Codex app-server is closing");
        const line = JSON.stringify(packet) + "\n";
        if (Buffer.byteLength(line) > MAX_FRAME_BYTES || this.child.stdin.writableLength > MAX_FRAME_BYTES) {
          throw new Error("Codex protocol output exceeds its bound");
        }
        this.child.stdin.write(line);
      }
      request(method, params, validate) {
        if (this.failure || this.ending) return Promise.reject(this.failure ?? new Error("Codex app-server is closing"));
        if (this.pending.size >= MAX_PENDING2) return Promise.reject(new Error("Too many pending Codex requests"));
        const id = `phux:${++this.nextId}`;
        const { promise, resolve: resolve2, reject } = Promise.withResolvers();
        const timer = setTimeout(() => {
          this.pending.delete(id);
          reject(new Error(`Codex ${method} response is unknown after timeout`));
        }, 3e4);
        this.pending.set(id, { resolve: resolve2, reject, timer });
        try {
          this.send({ id, method, params });
        } catch (error) {
          clearTimeout(timer);
          this.pending.delete(id);
          reject(error);
        }
        return promise.then((result) => {
          if (!validate(result)) throw new Error(`Codex ${method} response is incompatible with ${CODEX_VERSION}`);
          return result;
        });
      }
      fail(error) {
        if (this.failure) return;
        this.failure = error;
        for (const pending of this.pending.values()) {
          clearTimeout(pending.timer);
          pending.reject(error);
        }
        this.pending.clear();
        this.disconnected(error);
        if (!this.ending && this.child.exitCode === null) {
          this.child.kill("SIGTERM");
          const kill = setTimeout(() => this.child.kill("SIGKILL"), 5e3);
          void this.exited.then(() => clearTimeout(kill));
        }
      }
      async close() {
        if (this.ending) return this.exited;
        this.ending = true;
        this.fail(new Error("Codex app-server closed"));
        this.child.stdin.end();
        const terminate = setTimeout(() => this.child.kill("SIGTERM"), 1e3);
        const kill = setTimeout(() => this.child.kill("SIGKILL"), 5e3);
        try {
          await this.exited;
        } finally {
          clearTimeout(terminate);
          clearTimeout(kill);
        }
      }
    };
    CodexAdapter = class {
      constructor(context, child, ajv, validators) {
        this.context = context;
        this.ajv = ajv;
        this.validators = validators;
        this.mode = transcriptMode(context.env.PHUX_AGENT_TRANSCRIPT);
        this.rpc = new CodexRpc(child, (packet) => this.receive(packet), (error) => {
          const expected = this.closed;
          this.closed = true;
          for (const pending of this.questions.values()) pending.controller.abort();
          this.questions.clear();
          this.partials.clear();
          this.tools.clear();
          context.closeQuestions("Codex app-server disconnected");
          context.turn(void 0);
          context.connection(false);
          if (!expected) context.emit({ type: "notice", level: "error", text: error.message });
        });
      }
      context;
      ajv;
      validators;
      capabilities = { prompt: true, interrupt: true, questions: true, follow_up: true };
      providerSessionId;
      currentTurn;
      endedTurns = /* @__PURE__ */ new Set();
      questions = /* @__PURE__ */ new Map();
      partials = /* @__PURE__ */ new Map();
      tools = /* @__PURE__ */ new Map();
      mode;
      rpc;
      closed = false;
      async initialize() {
        const params = {
          clientInfo: { name: "phux", title: "phux Native Host", version: "1" },
          capabilities: { experimentalApi: false, requestAttestation: false, explicitGatewayOauth: true }
        };
        await this.rpc.request("initialize", params, this.validators.initialize);
        this.rpc.send({ method: "initialized" });
        let thread;
        if (this.context.resumeSessionId) {
          const resume = { threadId: this.context.resumeSessionId };
          const response = await this.rpc.request("thread/resume", resume, this.validators.resume);
          if (response.thread.id !== this.context.resumeSessionId) throw new Error("Codex resumed an unexpected thread");
          thread = response.thread;
        } else {
          const start = { cwd: this.context.cwd };
          thread = (await this.rpc.request("thread/start", start, this.validators.thread)).thread;
        }
        this.providerSessionId = thread.id;
        this.context.identity(thread.id);
        this.context.connection(true);
        for (const turn of thread.turns) {
          if (turn.status === "inProgress") this.started(turn.id);
        }
      }
      async prompt(text, _operationId) {
        if (this.closed || !this.providerSessionId) return { disposition: "refused", detail: "Codex is disconnected" };
        const params = { threadId: this.providerSessionId, input: [{ type: "text", text, text_elements: [] }] };
        try {
          const response = await this.rpc.request("turn/start", params, this.validators.turn);
          if (response.turn.status === "inProgress" && !this.endedTurns.has(response.turn.id)) this.started(response.turn.id);
          return { disposition: "admitted", detail: `Codex accepted turn ${response.turn.id}` };
        } catch (error) {
          return {
            disposition: error instanceof RpcRejected ? "refused" : "unknown",
            detail: error instanceof Error ? error.message : "Codex prompt admission is unknown"
          };
        }
      }
      async interrupt(turnId) {
        if (this.closed || !this.providerSessionId || turnId !== this.currentTurn) {
          return { disposition: "refused", detail: "That Codex turn is not active" };
        }
        const params = { threadId: this.providerSessionId, turnId };
        try {
          await this.rpc.request("turn/interrupt", params, this.validators.interrupt);
          return { disposition: "admitted", detail: "Codex acknowledged interruption; awaiting turn/completed" };
        } catch (error) {
          return {
            disposition: error instanceof RpcRejected ? "refused" : "unknown",
            detail: error instanceof Error ? error.message : "Codex interruption is unknown"
          };
        }
      }
      started(id) {
        if (this.endedTurns.has(id)) return;
        this.currentTurn = id;
        this.context.turn(id);
        this.context.emit({ type: "state", state: "working", turn_id: id });
      }
      receive(packet) {
        if ("id" in packet) {
          if (!requestId(packet.id)) return;
          if (!this.validators.request(packet)) {
            this.rpc.send({ id: packet.id, error: { code: -32601, message: "Unsupported Codex server request or incompatible parameters" } });
            return;
          }
          void this.answer(packet).catch(() => {
            this.context.emit({ type: "notice", level: "error", text: "Codex server request response failed" });
          });
          return;
        }
        if (!this.validators.notification(packet)) return;
        this.notification(packet);
      }
      notification(event) {
        const p = event.params;
        if ("threadId" in p && p.threadId !== null && this.providerSessionId && p.threadId !== this.providerSessionId) return;
        switch (event.method) {
          case "thread/started":
            break;
          // The correlated start/resume response owns identity.
          case "turn/started":
            this.started(event.params.turn.id);
            break;
          case "turn/completed": {
            const turn = event.params.turn;
            this.endedTurns.add(turn.id);
            if (this.endedTurns.size > MAX_PENDING2) {
              const oldest = this.endedTurns.values().next().value;
              if (oldest !== void 0) this.endedTurns.delete(oldest);
            }
            for (const item of turn.items) this.item(item, true);
            for (const [id, pending] of this.questions) {
              if (pending.closeWithTurn && pending.turnId === turn.id) {
                pending.controller.abort();
                this.questions.delete(id);
                this.context.emit({ type: "question_closed", id: questionId(id), reason: `Codex turn ${turn.id} ended` });
              }
            }
            if (turn.error) this.context.emit({ type: "notice", level: "error", text: keepHead(turn.error.message, 2048) });
            if (this.currentTurn === turn.id) {
              this.currentTurn = void 0;
              this.context.turn(void 0);
              this.context.emit({ type: "state", state: "done", turn_id: turn.id });
              this.partials.clear();
              this.tools.clear();
            }
            break;
          }
          case "thread/status/changed": {
            const status = event.params.status;
            if (status.type === "active") this.context.emit({ type: "state", state: status.activeFlags.length ? "blocked" : "working", turn_id: this.currentTurn });
            else if (status.type === "systemError") this.context.emit({ type: "notice", level: "error", text: "Codex thread reported a system error" });
            break;
          }
          case "serverRequest/resolved": {
            const pending = this.questions.get(event.params.requestId);
            if (pending) {
              pending.controller.abort();
              this.questions.delete(event.params.requestId);
              this.context.emit({ type: "question_closed", id: questionId(event.params.requestId), reason: "Codex resolved the server request" });
            }
            break;
          }
          case "item/started":
            this.item(event.params.item, false);
            break;
          case "item/completed":
            this.item(event.params.item, true);
            break;
          case "item/agentMessage/delta":
            this.delta(event.params.itemId, "assistant", event.params.delta);
            break;
          case "item/plan/delta":
            this.delta(event.params.itemId, "assistant", event.params.delta);
            break;
          case "item/reasoning/summaryTextDelta":
            this.delta(event.params.itemId, "thinking", event.params.delta);
            break;
          case "item/reasoning/textDelta":
            this.delta(event.params.itemId, "thinking", event.params.delta);
            break;
          case "item/commandExecution/outputDelta":
          case "item/fileChange/outputDelta":
            this.toolDelta(event.params.itemId, event.params.delta);
            break;
          case "item/mcpToolCall/progress":
            this.toolDelta(event.params.itemId, event.params.message + "\n");
            break;
          case "warning":
          case "guardianWarning":
            this.context.emit({ type: "notice", level: "warning", text: keepHead(event.params.message, 2048) });
            break;
          case "error":
            this.context.emit({ type: "notice", level: "error", text: keepHead(event.params.error.message, 2048) });
            break;
          case "thread/closed":
          case "thread/deleted":
          case "thread/archived":
            this.closed = true;
            for (const pending of this.questions.values()) pending.controller.abort();
            this.questions.clear();
            this.context.closeQuestions("Codex thread is unavailable");
            this.context.turn(void 0);
            this.context.connection(false);
            this.context.emit({ type: "notice", level: "warning", text: "Codex thread is unavailable" });
            break;
        }
      }
      publish(entry) {
        const bounded = transcriptData("codex", entry).entry;
        if (this.mode !== "off") this.context.emit({ type: "transcript", entry: bounded });
        return bounded;
      }
      delta(id, role, delta) {
        if (this.mode === "off") return;
        if (!this.partials.has(id) && this.partials.size >= MAX_PENDING2) return;
        const previous = this.partials.get(id);
        const all = (previous?.text ?? "") + delta;
        const text = keepHead(all, MAX_STREAM_BYTES);
        const truncated = Boolean(previous?.truncated) || text.length < all.length;
        this.partials.set(id, { role, text, truncated });
        this.publish({ id, role, text, truncated, final: false });
      }
      toolDelta(id, delta) {
        if (this.mode !== "full") return;
        const previous = this.tools.get(id);
        if (!previous?.tool) return;
        const all = previous.tool.output + delta;
        const output = keepTail(all, MAX_TOOL_OUTPUT_BYTES);
        const entry = this.publish({
          ...previous,
          truncated: previous.truncated || output.length < all.length,
          tool: { ...previous.tool, output }
        });
        this.tools.set(id, entry);
      }
      item(item, final) {
        if (this.mode === "off") return;
        const entry = (role, text) => this.publish({ id: item.id, role, text, truncated: false, final });
        if (final) this.partials.delete(item.id);
        switch (item.type) {
          case "userMessage":
            entry("user", item.content.filter((c) => c.type === "text").map((c) => c.text).join("\n"));
            return;
          case "agentMessage":
            entry("assistant", item.text);
            return;
          case "plan":
            entry("assistant", item.text);
            return;
          case "reasoning":
            entry("thinking", (item.summary.length ? item.summary : item.content).join("\n"));
            return;
          case "enteredReviewMode":
          case "exitedReviewMode":
            entry("system", item.review);
            return;
          case "contextCompaction":
            entry("system", "Codex compacted the thread context");
            return;
        }
        let name = item.type;
        let summary = "";
        let output = this.tools.get(item.id)?.tool?.output ?? "";
        let failed = false;
        const running = "status" in item && item.status === "inProgress";
        switch (item.type) {
          case "commandExecution":
            name = "command";
            summary = item.command;
            output = item.aggregatedOutput ?? output;
            failed = item.status === "failed" || item.status === "declined";
            break;
          case "fileChange":
            name = "file_change";
            summary = item.changes.map((change) => change.path).join(", ");
            failed = item.status === "failed" || item.status === "declined";
            break;
          case "mcpToolCall":
            name = `${item.server}/${item.tool}`;
            summary = summarizeArgs(item.arguments);
            failed = item.status === "failed" || item.error !== null;
            if (this.mode === "full") output = item.result?.content.flatMap((c) => object2(c) && c.type === "text" && typeof c.text === "string" ? [c.text] : []).join("\n") ?? item.error?.message ?? output;
            break;
          case "dynamicToolCall":
            name = item.tool;
            summary = summarizeArgs(item.arguments);
            failed = item.success === false || item.status === "failed";
            if (this.mode === "full") output = item.contentItems?.flatMap((c) => c.type === "inputText" ? [c.text] : []).join("\n") ?? output;
            break;
          case "collabAgentToolCall":
            name = item.tool;
            summary = item.receiverThreadIds.join(", ");
            failed = item.status === "failed";
            break;
          case "webSearch":
            summary = item.query ?? "";
            break;
          case "imageView":
            summary = item.path;
            break;
          case "subAgentActivity":
            summary = item.agentPath;
            break;
          case "functionCallOutput":
            name = item.namespace ? `${item.namespace}/${item.name}` : item.name;
            if (this.mode === "full") output = typeof item.output === "string" ? item.output : item.output.flatMap((c) => c.type === "input_text" ? [c.text] : []).join("\n");
            break;
          case "imageGeneration":
            summary = item.savedPath ?? "Image generation";
            failed = item.failure !== null;
            break;
          case "sleep":
            name = "clock.sleep";
            summary = `${item.durationMs} ms`;
            break;
          default:
            return;
        }
        const projected = this.publish({
          id: item.id,
          role: "tool",
          text: "",
          truncated: false,
          final,
          tool: { name, call_id: item.id, summary, status: failed ? "error" : !final || running ? "running" : "ok", output: this.mode === "full" ? output : "" }
        });
        if (final) this.tools.delete(item.id);
        else if (this.tools.size < MAX_PENDING2) this.tools.set(item.id, projected);
      }
      async ask(id, question, turnId, closeWithTurn = true) {
        if (this.questions.size >= MAX_PENDING2 || Buffer.byteLength(JSON.stringify(question)) > MAX_QUESTION_BYTES) {
          throw new Error("Codex question exceeds native UI bounds");
        }
        if (this.questions.has(id)) throw new Error("Duplicate active Codex request ID");
        const pending = { controller: new AbortController(), turnId, closeWithTurn };
        this.questions.set(id, pending);
        try {
          const response = await this.context.question(question, pending.controller.signal);
          if (pending.controller.signal.aborted || this.closed) throw new QuestionResolved();
          return response;
        } catch (error) {
          if (pending.controller.signal.aborted) throw new QuestionResolved();
          throw error;
        }
      }
      async choose(request, decisions, cancelled, title, message) {
        const response = await this.ask(request.id, {
          id: questionId(request.id),
          kind: "permission",
          title,
          message,
          options: decisions.map((decision) => ({ id: typeof decision === "string" ? decision : JSON.stringify(decision), label: typeof decision === "string" ? decision : JSON.stringify(decision) })),
          questions: [],
          fields: json({ rpc_request_id: request.id, ...request.params })
        }, "turnId" in request.params ? request.params.turnId : void 0);
        if (object2(response) && response.cancelled === true) return cancelled;
        if (!object2(response) || typeof response.option_id !== "string") throw new Error("Codex permission requires an explicit option ID");
        const selected = decisions.find((decision) => (typeof decision === "string" ? decision : JSON.stringify(decision)) === response.option_id);
        if (selected === void 0) throw new Error("Unknown Codex permission option");
        return selected;
      }
      async answer(request) {
        try {
          if ("threadId" in request.params && this.providerSessionId && request.params.threadId !== this.providerSessionId) throw new Error("Codex request targets another thread");
          let result;
          switch (request.method) {
            case "item/commandExecution/requestApproval": {
              const p = request.params;
              const decisions = ["accept", "acceptForSession", "decline", "cancel"];
              if (p.proposedExecpolicyAmendment) decisions.push({ acceptWithExecpolicyAmendment: { execpolicy_amendment: p.proposedExecpolicyAmendment } });
              for (const amendment of p.proposedNetworkPolicyAmendments ?? []) decisions.push({ applyNetworkPolicyAmendment: { network_policy_amendment: amendment } });
              result = { decision: await this.choose(request, decisions, "cancel", "Codex command permission", p.command ?? p.reason ?? p.kind) };
              break;
            }
            case "item/fileChange/requestApproval": {
              const decisions = ["accept", "acceptForSession", "decline", "cancel"];
              result = { decision: await this.choose(request, decisions, "cancel", "Codex file permission", request.params.reason ?? request.params.itemId) };
              break;
            }
            case "item/permissions/requestApproval": {
              const p = request.params;
              const granted = { ...p.permissions.network ? { network: p.permissions.network } : {}, ...p.permissions.fileSystem ? { fileSystem: p.permissions.fileSystem } : {} };
              const decisions = [
                { permissions: granted, scope: "turn" },
                { permissions: granted, scope: "session" },
                { permissions: {}, scope: "turn" }
              ];
              const selected = await this.choose(request, decisions, { permissions: {}, scope: "turn" }, "Codex permissions", p.reason ?? "Review the requested permissions and scope");
              if (!this.validators.permissions(selected)) throw new Error("Invalid Codex permissions response");
              result = selected;
              break;
            }
            case "item/tool/requestUserInput": {
              const p = request.params;
              if (p.questions.some((q) => q.isSecret)) throw new Error("Secret Codex user input is unsupported");
              const response = await this.ask(request.id, {
                id: questionId(request.id),
                kind: "ask",
                title: "Codex user input",
                message: "",
                options: [],
                questions: p.questions.map((q) => ({
                  id: q.id,
                  header: q.header,
                  question: q.question,
                  multi: false,
                  options: (q.options ?? []).map((option) => ({ id: option.label, label: option.label, description: option.description }))
                })),
                fields: json({ rpc_request_id: request.id, ...p })
              }, p.turnId, p.isBlocking);
              if (object2(response) && response.cancelled === true) {
                result = { answers: {} };
                break;
              }
              if (!object2(response) || !Array.isArray(response.answers)) throw new Error("Invalid Codex user input response");
              const answers = {};
              for (const answer of response.answers) {
                if (!object2(answer) || typeof answer.id !== "string" || !Array.isArray(answer.selected_options)) throw new Error("Invalid Codex answer");
                const q = p.questions.find((candidate) => candidate.id === answer.id);
                if (!q || Object.hasOwn(answers, q.id) || answer.selected_options.length > 1) throw new Error("Invalid Codex question ID or multiple selections");
                const chosen = [];
                for (const option of answer.selected_options) {
                  if (typeof option !== "string" || !q.options?.some((candidate) => candidate.label === option)) throw new Error("Unknown Codex input option");
                  chosen.push(option);
                }
                if (answer.custom_input !== void 0) {
                  if (typeof answer.custom_input !== "string" || !q.isOther && q.options !== null) throw new Error("Custom Codex input is unsupported for this question");
                  if (chosen.length) throw new Error("Choose an option or custom input, not both");
                  chosen.push(answer.custom_input);
                }
                if (!chosen.length) throw new Error("Codex input question is unanswered");
                Object.defineProperty(answers, q.id, { value: { answers: chosen }, enumerable: true });
              }
              if (Object.keys(answers).length !== p.questions.length) throw new Error("Codex input response is incomplete");
              result = { answers };
              break;
            }
            case "mcpServer/elicitation/request": {
              const p = request.params;
              if (p.mode !== "form") throw new Error(`Codex MCP ${p.mode} UI is unsupported`);
              const schema = json(p.requestedSchema);
              if (!object2(schema) || schema.type !== "object" || !object2(schema.properties)) throw new Error("Unsupported Codex form schema");
              for (const field of Object.values(schema.properties)) {
                if (!object2(field) || !["string", "boolean", "number", "integer", "array"].includes(String(field.type))) throw new Error("Unsupported Codex form field");
                if (field.type === "array" && (!object2(field.items) || field.items.type !== "string")) throw new Error("Unsupported Codex form array");
              }
              const validate = this.ajv.compile(p.requestedSchema);
              const response = await this.ask(request.id, {
                id: questionId(request.id),
                kind: "form",
                title: p.serverName,
                message: p.message,
                options: [],
                questions: [],
                fields: schema
              }, p.turnId, false);
              if (object2(response) && response.cancelled === true) {
                result = { action: "cancel", content: null, _meta: null };
                break;
              }
              if (!object2(response) || !object2(response.values) || !validate(response.values)) throw new Error("Codex form response does not satisfy the provider schema");
              const accepted = { action: "accept", content: json(response.values), _meta: null };
              if (!this.validators.elicitation(accepted)) throw new Error("Invalid Codex elicitation response");
              result = accepted;
              break;
            }
            default:
              throw new Error(`Unsupported Codex server request: ${request.method}`);
          }
          if (!this.closed) this.rpc.send({ id: request.id, result });
        } catch (error) {
          if (this.closed || error instanceof QuestionResolved) return;
          this.rpc.send({ id: request.id, error: { code: -32602, message: error instanceof Error ? error.message : "Codex native UI request refused" } });
          this.context.emit({ type: "notice", level: "warning", text: "Codex native UI request was refused or cancelled" });
        }
      }
      async close() {
        this.closed = true;
        for (const pending of this.questions.values()) pending.controller.abort();
        this.questions.clear();
        this.context.closeQuestions("Codex app-server closed");
        this.context.connection(false);
        await this.rpc.close();
      }
    };
  }
});

// src/amp.ts
var amp_exports = {};
__export(amp_exports, {
  AmpTranscript: () => AmpTranscript,
  createAdapter: () => createAdapter5
});
import { createRequire } from "node:module";
import { readFile as readFile2, access as access2, open } from "node:fs/promises";
import { dirname, join as join2 } from "node:path";
import { constants as constants2 } from "node:fs";
async function createAdapter5(context) {
  if (context.command.length !== 1 || context.command[0] !== "node") throw new Error('Amp in-process SDK requires command ["node"]; additional CLI arguments are unsupported and were not applied');
  const { execute: execute2, createUserMessage } = await import("@ampcode/sdk");
  const require2 = createRequire(join2(context.runtime, "package.json"));
  const cliManifest = require2.resolve("@ampcode/cli/package.json");
  const manifest = JSON.parse(await readFile2(cliManifest, "utf8"));
  if (typeof manifest !== "object" || manifest === null || !("version" in manifest) || manifest.version !== CLI_VERSION) {
    throw new Error("Amp private runtime CLI version does not match the reviewed runtime lock");
  }
  const binaryPath = join2(dirname(cliManifest), "bin", "amp.exe");
  await access2(binaryPath, constants2.X_OK);
  const binary = await open(binaryPath, "r");
  try {
    const header = Buffer.alloc(4);
    const { bytesRead } = await binary.read(header, 0, 4, 0);
    const magic = header.readUInt32BE(0);
    const native = magic === 2135247942 || magic === 4277009102 || magic === 4277009103 || magic === 3472551422 || magic === 3489328638 || magic === 3405691582 || magic === 3199925962 || header.readUInt16BE(0) === 19802;
    if (bytesRead !== 4 || !native) throw new Error("Amp native runtime asset was not installed; refusing SDK wrapper fallback");
  } finally {
    await binary.close();
  }
  const projection = new AmpTranscript(context);
  let active;
  let closed = false;
  context.emit({ type: "notice", level: "info", text: `Amp SDK ${SDK_VERSION}: interactive permission replies and provider-confirmed input/cancel acknowledgments are unavailable. Cancellation terminates the current SDK execution; later prompts continue its actual thread ID.` });
  return {
    capabilities: { prompt: true, interrupt: true, questions: false, follow_up: false },
    get providerSessionId() {
      return projection.confirmed ? projection.sessionId : void 0;
    },
    async prompt(text, operationId) {
      if (closed) return { disposition: "refused", detail: "Amp adapter is closed" };
      if (active) return { disposition: "refused", detail: "Amp execution is active; wait for its result before continuing the same thread" };
      const requestId2 = `${context.activation}:${operationId}`;
      const abort = new AbortController();
      async function* input() {
        yield createUserMessage(text, { requestId: requestId2 });
      }
      const done = (async () => {
        try {
          let resultSeen = false;
          for await (const message of execute2({ prompt: input(), options: { cwd: context.cwd, ...projection.sessionId ? { continue: projection.sessionId } : {}, dangerouslyAllowAll: false }, signal: abort.signal })) {
            if (message.type === "result") resultSeen = true;
            projection.emit(message, requestId2);
          }
          if (!resultSeen) context.emit({ type: "notice", level: "warning", text: "Amp SDK stream ended without a provider result; no command retry was attempted." });
        } catch {
          context.emit({ type: "notice", level: abort.signal.aborted ? "warning" : "error", text: abort.signal.aborted ? "Amp SDK execution aborted locally; no provider semantic cancellation acknowledgment is available." : "Amp SDK execution failed; no input retry was attempted." });
          context.emit({ type: "state", state: "done" });
        } finally {
          active = void 0;
          context.turn(void 0);
          projection.tools.clear();
          if (!projection.confirmed) {
            context.emit({ type: "notice", level: "error", text: "Amp ended before confirming a thread ID. Controls are unavailable rather than silently creating a replacement conversation." });
            context.connection(false);
          }
        }
      })();
      active = { requestId: requestId2, abort, done };
      context.turn(requestId2);
      return { disposition: "queued", detail: "Input queued to Amp SDK execute; this SDK exposes no provider admission receipt" };
    },
    async interrupt(turnId) {
      if (!active || active.requestId !== turnId) return { disposition: "refused", detail: "Amp input request is no longer active" };
      active.abort.abort();
      return { disposition: "queued", detail: "AbortSignal submitted to Amp SDK; process cancellation is not provider-confirmed turn interruption" };
    },
    async close() {
      closed = true;
      const current = active;
      current?.abort.abort();
      await current?.done;
    }
  };
}
var SDK_VERSION, CLI_VERSION, AmpTranscript;
var init_amp = __esm({
  "src/amp.ts"() {
    "use strict";
    init_transcript();
    SDK_VERSION = "0.1.0-20260918210405-g81edbf0";
    CLI_VERSION = "0.0.1792008042-gef0030";
    AmpTranscript = class {
      constructor(context) {
        this.context = context;
        this.sessionId = context.resumeSessionId;
      }
      context;
      sessionId;
      confirmed = false;
      tools = /* @__PURE__ */ new Map();
      userSequence = 0;
      emit(message, requestId2) {
        const context = this.context;
        if (message.session_id) {
          if (this.sessionId !== void 0 && this.sessionId !== message.session_id) throw new Error("Amp changed provider thread identity");
          this.sessionId = message.session_id;
          this.confirmed = true;
          context.identity(this.sessionId);
        }
        if (message.type === "system") {
          context.emit({ type: "state", state: "working", turn_id: requestId2 });
          return;
        }
        if (message.type === "assistant") {
          const text = message.message.content.filter((block) => block.type === "text").map((block) => block.text).join("");
          if (text) context.emit({ type: "transcript", entry: { id: message.message.id, role: "assistant", text, final: true, truncated: false } });
          for (const block of message.message.content) {
            if (block.type !== "tool_use") continue;
            const tool = { call_id: block.id, name: block.name, summary: summarizeArgs(block.input), status: "running", output: "" };
            this.tools.set(block.id, tool);
            context.emit({ type: "transcript", entry: { id: block.id, role: "tool", text: "", tool, final: false, truncated: false } });
          }
          return;
        }
        if (message.type === "user") {
          const text = message.message.content.filter((block) => block.type === "text").map((block) => block.text).join("");
          if (text) context.emit({ type: "transcript", entry: { id: `${requestId2}:user:${++this.userSequence}`, role: "user", text, final: true, truncated: false } });
          for (const block of message.message.content) {
            if (block.type !== "tool_result") continue;
            const prior = this.tools.get(block.tool_use_id);
            const tool = { call_id: block.tool_use_id, name: prior?.name ?? "tool", summary: prior?.summary ?? "", status: block.is_error ? "error" : "ok", output: transcriptMode(context.env.PHUX_AGENT_TRANSCRIPT) === "full" ? block.content : "" };
            context.emit({ type: "transcript", entry: { id: block.tool_use_id, role: "tool", text: "", tool, final: true, truncated: false } });
            this.tools.delete(block.tool_use_id);
          }
          return;
        }
        for (const denial of message.permission_denials ?? []) {
          context.emit({ type: "notice", level: "warning", text: `Amp denied permission for ${denial.slice(0, 256)}. This SDK does not expose an interactive permission-reply API.` });
        }
        if (message.is_error) context.emit({ type: "notice", level: "error", text: "Amp returned an execution error; provider diagnostics remain on the executing host." });
        context.turn(void 0);
        context.emit({ type: "state", state: "done" });
      }
    };
  }
});

// src/bridge.ts
var bridge_exports = {};
__export(bridge_exports, {
  nativeUpdate: () => nativeUpdate,
  parseCommand: () => parseCommand,
  runBridge: () => runBridge
});
import { createHash } from "node:crypto";
function nativeUpdate(provider2, update, mode) {
  if (update.type === "view") return { ...update, view: { ...update.view, backend: "node" } };
  if (update.type !== "transcript") return update;
  if (mode === "off") return void 0;
  const source = update.entry.tool && mode !== "full" ? { ...update.entry, tool: { ...update.entry.tool, output: "" } } : update.entry;
  let data = transcriptData(provider2, source);
  for (; ; ) {
    const wire = { type: "transcript", entry: data };
    const over = Buffer.byteLength(JSON.stringify(wire)) - 14 * 1024;
    if (over <= 0) return wire;
    const entry = data.entry;
    if (entry.text.length) {
      data = { ...data, entry: { ...entry, truncated: true, text: keepHead(entry.text, Math.max(0, Buffer.byteLength(entry.text) - over)) } };
    } else if (entry.tool?.output.length) {
      data = { ...data, entry: { ...entry, truncated: true, tool: { ...entry.tool, output: keepTail(entry.tool.output, Math.max(0, Buffer.byteLength(entry.tool.output) - over)) } } };
    } else {
      throw new Error("Native transcript metadata exceeds update bounds");
    }
  }
}
function parseCommand(value) {
  if (value === null || typeof value !== "object" || Array.isArray(value)) throw new Error("Packet must be an object");
  const packet = value;
  if (packet.type === "shutdown") return { type: "shutdown" };
  if (packet.type !== "command" || typeof packet.operation_id !== "string" || !/^(0|[1-9][0-9]{0,19})$/.test(packet.operation_id) || BigInt(packet.operation_id) > 18446744073709551615n || typeof packet.activation !== "string" || packet.activation.length > 256 || packet.intent === null || typeof packet.intent !== "object" || Array.isArray(packet.intent)) throw new Error("Invalid native command envelope");
  const input = packet.intent;
  let intent;
  if (input.type === "prompt" && typeof input.text === "string" && input.text.trim().length > 0) intent = { type: "prompt", text: input.text };
  else if (input.type === "interrupt" && typeof input.turn_id === "string" && input.turn_id.length > 0 && input.turn_id.length <= 256) intent = { type: "interrupt", turn_id: input.turn_id };
  else if (input.type === "answer" && typeof input.question_id === "string" && input.question_id.length > 0 && input.question_id.length <= 256 && input.response !== void 0) intent = { type: "answer", question_id: input.question_id, response: input.response };
  else throw new Error("Invalid native intent");
  return { operation_id: packet.operation_id, activation: packet.activation, intent };
}
async function runBridge(startup, writeProtocol) {
  const { Ajv } = await import("ajv");
  const { default: addFormats } = await import("ajv-formats");
  const ajv = new Ajv({ strict: false, allErrors: false, validateFormats: true, ownProperties: true });
  addFormats(ajv);
  let adapter;
  let providerId;
  let turnId;
  let connected = true;
  let available = true;
  let closing;
  let failed = false;
  let capabilities2 = { prompt: false, interrupt: false, questions: false, follow_up: false };
  let bufferedBytes = 0;
  let writable = true;
  const output = [];
  const fatal = (message) => {
    if (failed) return;
    failed = true;
    process.exitCode = 1;
    writeProtocol(JSON.stringify({ type: "failed", message }) + "\n");
    void shutdown();
  };
  const flush = () => {
    while (writable && output.length) {
      const line = output.shift();
      bufferedBytes -= Buffer.byteLength(line);
      writable = writeProtocol(line);
    }
  };
  const send = (packet) => {
    if (failed) return;
    const line = JSON.stringify(packet) + "\n";
    const bytes = Buffer.byteLength(line);
    if (bytes > MAX_PACKET_BYTES || bufferedBytes + bytes > MAX_BUFFER_BYTES) {
      fatal("Native SDK output exceeded bounded protocol capacity; controls closed without retrying input");
      return;
    }
    output.push(line);
    bufferedBytes += bytes;
    flush();
  };
  const publishView = () => send({ type: "update", update: { type: "view", view: { activation: startup.activation, backend: "node", capabilities: capabilities2, connected: connected && available, ...turnId ? { turn_id: turnId } : {}, ...providerId ? { provider_session_id: providerId } : {}, questions: [...questions.pending.values()].map((item) => item.question) } } });
  const emit = (update) => {
    if (update.type === "question_closed") questions.closed(update.id);
    const wire = nativeUpdate(startup.provider, update, transcriptMode(process.env.PHUX_AGENT_TRANSCRIPT));
    if (!wire) return;
    send({ type: "update", update: wire });
    if (update.type === "question_closed") publishView();
  };
  const questions = new QuestionRegistry((schema) => ajv.compile(schema), (question) => {
    capabilities2 = { ...capabilities2, questions: true };
    send({ type: "update", update: { type: "question_opened", question } });
    publishView();
  }, (id, reason) => {
    send({ type: "update", update: { type: "question_closed", id, reason } });
  });
  const context = {
    ...startup,
    env: { ...process.env },
    emit,
    connection(value) {
      available = value;
      publishView();
    },
    identity(id) {
      if (!id || id.length > 256) throw new Error("Invalid provider session identity");
      providerId = id;
      publishView();
    },
    turn(id) {
      turnId = id;
      publishView();
    },
    question: (question, signal) => questions.open(question, signal),
    closeQuestions(reason) {
      questions.closeAll(reason);
      publishView();
    }
  };
  const ledger = /* @__PURE__ */ new Map();
  const dispatch = async (command2) => {
    if (command2.activation !== startup.activation) return { operation_id: command2.operation_id, disposition: "refused", detail: "Native activation is stale" };
    if (!connected || !available) return { operation_id: command2.operation_id, disposition: "refused", detail: "SDK source is unavailable" };
    const intent = command2.intent;
    if (intent.type === "prompt" && Buffer.byteLength(intent.text) > 12 * 1024) return { operation_id: command2.operation_id, disposition: "refused", detail: "Prompt exceeds the native SDK 12288-byte supported limit; input was not dispatched" };
    if (intent.type === "answer") {
      const answer = questions.answer(intent.question_id, intent.response);
      return { operation_id: command2.operation_id, disposition: answer.accepted ? "queued" : "refused", detail: answer.detail };
    }
    if (!adapter) return { operation_id: command2.operation_id, disposition: "refused", detail: "SDK initialization has not completed" };
    if (intent.type === "interrupt" && (!capabilities2.interrupt || turnId !== intent.turn_id)) return { operation_id: command2.operation_id, disposition: "refused", detail: "Provider turn is stale or interruption is unsupported" };
    if (intent.type === "prompt" && !capabilities2.prompt) return { operation_id: command2.operation_id, disposition: "refused", detail: "Provider prompts are unsupported" };
    try {
      const outcome = intent.type === "prompt" ? await adapter.prompt(intent.text, command2.operation_id) : await adapter.interrupt(intent.turn_id);
      return { operation_id: command2.operation_id, ...outcome };
    } catch {
      return { operation_id: command2.operation_id, disposition: "unknown", detail: "SDK command failed after dispatch; input was not retried" };
    }
  };
  const commandReceived = (command2) => {
    const fingerprint = createHash("sha256").update(JSON.stringify(command2)).digest("hex");
    const previous = ledger.get(command2.operation_id);
    if (previous) {
      if (previous.fingerprint !== fingerprint) send({ type: "result", result: { operation_id: command2.operation_id, disposition: "refused", detail: "Operation ID was reused with a changed command" } });
      else void previous.result.then((result2) => send({ type: "result", result: result2 }));
      return;
    }
    if (ledger.size >= 4096) {
      send({ type: "result", result: { operation_id: command2.operation_id, disposition: "refused", detail: "Native operation ledger is full; input was not dispatched" } });
      return;
    }
    let complete;
    const result = new Promise((resolve2) => {
      complete = resolve2;
    });
    ledger.set(command2.operation_id, { fingerprint, result });
    void dispatch(command2).then(complete);
    void result.then((result2) => send({ type: "result", result: result2 }));
  };
  function shutdown() {
    if (closing) return closing;
    connected = false;
    questions.closeAll("host_closed");
    publishView();
    process.stdin.pause();
    closing = (async () => {
      await adapter?.close();
      process.stdin.removeListener("data", input);
      process.stdin.removeListener("end", ended);
      process.removeListener("SIGTERM", terminated);
      process.removeListener("SIGINT", terminated);
      flush();
    })();
    return closing;
  }
  let pending = Buffer.alloc(0);
  function input(chunk) {
    if (!connected) return;
    pending = Buffer.concat([pending, chunk]);
    for (; ; ) {
      const end = pending.indexOf(10);
      if (end < 0) break;
      if (end >= MAX_PACKET_BYTES) {
        fatal("Native SDK input packet exceeds protocol bounds");
        return;
      }
      const line = pending.subarray(0, end).toString("utf8");
      pending = pending.subarray(end + 1);
      try {
        const packet = parseCommand(JSON.parse(line));
        if ("type" in packet) {
          void shutdown();
          return;
        }
        commandReceived(packet);
      } catch {
        fatal("Malformed native SDK input packet; no terminal fallback is available");
        return;
      }
    }
    if (pending.length >= MAX_PACKET_BYTES) fatal("Native SDK input packet exceeds protocol bounds");
  }
  const ended = () => {
    if (pending.length) fatal("Native SDK command stream ended mid-packet");
    else void shutdown();
  };
  const terminated = () => {
    void shutdown();
  };
  process.stdout.on("drain", () => {
    writable = true;
    flush();
  });
  process.stdout.on("error", () => {
    failed = true;
    process.exitCode = 1;
    void shutdown();
  });
  process.stdin.on("data", input);
  process.stdin.on("end", ended);
  process.on("SIGTERM", terminated);
  process.on("SIGINT", terminated);
  try {
    if ((startup.provider === "pi" || startup.provider === "amp") && (startup.command.length !== 1 || startup.command[0] !== "node")) {
      fatal('In-process Pi/Amp SDK requires command ["node"]; additional CLI arguments are unsupported and were not applied');
      return;
    }
    let module;
    switch (startup.provider) {
      case "pi":
        module = await Promise.resolve().then(() => (init_pi(), pi_exports));
        break;
      case "claude":
        module = await Promise.resolve().then(() => (init_claude(), claude_exports));
        break;
      case "opencode":
        module = await Promise.resolve().then(() => (init_opencode(), opencode_exports));
        break;
      case "codex":
        module = await Promise.resolve().then(() => (init_codex(), codex_exports));
        break;
      case "amp":
        module = await Promise.resolve().then(() => (init_amp(), amp_exports));
        break;
      default:
        throw new Error("Unsupported native SDK provider");
    }
    adapter = await module.createAdapter(context);
    if (!connected) {
      await adapter.close();
      return;
    }
    capabilities2 = adapter.capabilities;
    providerId = adapter.providerSessionId ?? providerId;
    send({ type: "ready", ...providerId ? { provider_session_id: providerId } : {}, capabilities: capabilities2 });
    publishView();
  } catch {
    fatal("Native SDK initialization failed; verify isolated runtime assets and compatible provider executable on the executing host");
  }
}
var MAX_PACKET_BYTES, MAX_BUFFER_BYTES;
var init_bridge = __esm({
  "src/bridge.ts"() {
    "use strict";
    init_transcript();
    init_questions();
    MAX_PACKET_BYTES = 16 * 1024;
    MAX_BUFFER_BYTES = 8 * 1024 * 1024;
  }
});

// src/index.ts
import { registerHooks } from "node:module";
import { pathToFileURL } from "node:url";
import { resolve, isAbsolute as isAbsolute3, join as join3 } from "node:path";
import { Console } from "node:console";
var provider = process.env.PHUX_NATIVE_PROVIDER;
var activation = process.env.PHUX_NATIVE_ACTIVATION;
var cwd = process.env.PHUX_NATIVE_CWD;
var runtime = process.env.PHUX_NATIVE_SDK_RUNTIME;
var resumeSessionId = process.env.PHUX_NATIVE_SESSION_ID;
var command;
try {
  command = JSON.parse(process.env.PHUX_NATIVE_COMMAND ?? "[]");
} catch {
  command = null;
}
var providers = ["pi", "claude", "opencode", "codex", "amp"];
if (!provider || !providers.includes(provider) || !activation || activation.length > 256 || !cwd || !isAbsolute3(cwd) || !runtime || !isAbsolute3(runtime) || !Array.isArray(command) || !command.every((part) => typeof part === "string") || command.length > 64) {
  process.stdout.write(JSON.stringify({ type: "failed", message: "Invalid native SDK startup configuration" }) + "\n");
  process.exitCode = 1;
} else {
  const startup = { provider, activation, cwd, runtime: resolve(runtime), command, ...resumeSessionId ? { resumeSessionId } : {} };
  const writeProtocol = process.stdout.write.bind(process.stdout);
  process.stdout.write = process.stderr.write.bind(process.stderr);
  globalThis.console = new Console({ stdout: process.stderr, stderr: process.stderr });
  for (const key of Object.keys(process.env)) {
    if (key.startsWith("PHUX_NATIVE_") || key.startsWith("PHUX_CONTROLLER_") || key === "PHUX_BINDING_PROOF" || key === "AMP_DEBUG") delete process.env[key];
  }
  process.env.PHUX_AGENT_REPORTING = "0";
  const ownUrl = import.meta.url;
  const runtimeUrl = pathToFileURL(join3(startup.runtime, "package.json")).href;
  registerHooks({
    resolve(specifier, context, nextResolve) {
      if (context.parentURL === ownUrl && !specifier.startsWith(".") && !specifier.startsWith("/") && !specifier.startsWith("node:") && !specifier.startsWith("file:")) {
        return nextResolve(specifier, { ...context, parentURL: runtimeUrl });
      }
      return nextResolve(specifier, context);
    }
  });
  try {
    const { runBridge: runBridge2 } = await Promise.resolve().then(() => (init_bridge(), bridge_exports));
    await runBridge2(startup, writeProtocol);
  } catch {
    writeProtocol(JSON.stringify({ type: "failed", message: "Native SDK runtime could not be loaded from its reviewed isolated installation" }) + "\n");
    process.exitCode = 1;
  }
}
