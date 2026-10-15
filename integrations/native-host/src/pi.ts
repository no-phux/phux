import { randomUUID } from 'node:crypto';
import type {
  AgentSession, AgentSessionEvent, ExtensionUIContext, ExtensionUIDialogOptions, SessionManager,
} from '@earendil-works/pi-coding-agent';
import { summarizeArgs, transcriptData, transcriptMode } from '../../runtime/src/transcript.js';
import type { AdapterContext, Json, NativeAdapter, NativeQuestion, TranscriptEntry } from './types.js';

// Compatibility: official @earendil-works/pi-coding-agent 1.1.0, Node >=22.19.
// Keep the SDK's complete private dependency tree (including WASM/esbuild assets).
// The dedicated bridge must install context.env as process.env BEFORE importing
// this module: Pi's in-process SDK has no per-session environment override.
// No CLI, RpcClient/private response writer, global settings mutation or update.
export async function createAdapter(context: AdapterContext): Promise<NativeAdapter> {
  if (context.command.length !== 1 || context.command[0] !== 'node') throw new Error('Pi in-process SDK requires command ["node"]; additional CLI arguments are unsupported and were not applied');
  if (Object.keys(process.env).some(key => process.env[key] !== context.env[key]) ||
      Object.keys(context.env).some(key => context.env[key] !== process.env[key])) {
    throw new Error('Pi requires the dedicated worker environment to match AdapterContext.env');
  }
  // SDK import must follow isolated-runtime resolution hook and sanitized
  // process environment; a hoisted static value import cannot do that.
  const { createAgentSession, DefaultResourceLoader, getAgentDir,
    hasTrustRequiringProjectResources, ProjectTrustStore, SessionManager, SettingsManager,
  } = await import('@earendil-works/pi-coding-agent');
  const agentDir = getAgentDir();
  const ui = createPiUI(context);
  // The SDK factory otherwise defaults projectTrusted=true. Resolve trust before
  // loading project extensions/settings; new consent is session-only, never saved.
  const settingsManager = SettingsManager.create(context.cwd, agentDir, { projectTrusted: false });
  const resourceLoader = new DefaultResourceLoader({ cwd: context.cwd, agentDir, settingsManager });
  const trustStore = new ProjectTrustStore(agentDir);
  await resourceLoader.reload({ resolveProjectTrust: async () => {
    const saved = trustStore.get(context.cwd);
    if (saved !== null) return saved;
    if (!hasTrustRequiringProjectResources(context.cwd)) return false;
    return ui.confirm('Trust this Pi project for this session?',
      `Allow project-local Pi settings, extensions and skills in ${context.cwd}?`);
  } });
  const sessionDir = settingsManager.getSessionDir();
  let sessionManager: SessionManager;
  if (context.resumeSessionId !== undefined) {
    const path = SessionManager.findById(context.cwd, context.resumeSessionId, sessionDir);
    if (!path) throw new Error('The requested Pi session ID was not found for this working directory');
    sessionManager = SessionManager.open(path, sessionDir, context.cwd);
    if (sessionManager.getSessionId() !== context.resumeSessionId) {
      throw new Error('Pi resumed session identity does not match the requested session ID');
    }
  } else {
    sessionManager = SessionManager.create(context.cwd, sessionDir);
  }
  const { session, modelFallbackMessage } = await createAgentSession({
    cwd: context.cwd, agentDir, sessionManager, settingsManager, resourceLoader,
  });
  const adapter = attachPiSession(context, session);
  try {
    await session.bindExtensions({
      uiContext: ui, mode: 'rpc',
      abortHandler: () => { void session.abort().catch(() => {
        context.emit({ type: 'notice', level: 'error', text: 'Pi extension abort failed.' });
      }); },
      onError: () => context.emit({ type: 'notice', level: 'error', text: 'Pi extension failed.' }),
    });
  } catch (error) {
    await adapter.close();
    throw error;
  }
  if (modelFallbackMessage) context.emit({ type: 'notice', level: 'warning',
    text: 'Pi reports a model selection fallback; check the configured model and host authentication.' });
  return adapter;
}

function object(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

/** Implements the actual 1.1.0 UI surface, not RPC response packets. */
export function createPiUI(context: AdapterContext): ExtensionUIContext {
  const unsupported = (method: string): never => {
    throw new Error(`Pi native UI does not support terminal-only ${method}`);
  };
  const notice = (text: string, level: 'info' | 'warning' | 'error' = 'info'): void => {
    context.emit({ type: 'notice', level, text });
  };
  async function ask<T>(kind: NativeQuestion['kind'], title: string, message: string,
    options: NativeQuestion['options'], fields: Json, decode: (answer: Json) => T,
    opts?: ExtensionUIDialogOptions): Promise<T | undefined> {
    if (opts?.signal?.aborted) return undefined;
    if (opts?.timeout !== undefined && (!Number.isFinite(opts.timeout) || opts.timeout < 0)) {
      throw new Error('Pi dialog timeout must be finite and nonnegative');
    }
    const controller = new AbortController();
    const cancel = (): void => controller.abort();
    opts?.signal?.addEventListener('abort', cancel, { once: true });
    const timer = opts?.timeout === undefined ? undefined : setTimeout(cancel, opts.timeout);
    const id = randomUUID();
    let reason = 'Pi UI dialog failed';
    try {
      // Pi's in-process UI API has no upstream request IDs: this UUID is ONLY a
      // host dialog correlation key, not a fabricated provider/session identity.
      const response = await context.question({ id, kind, title, message,
        options, questions: [], fields }, controller.signal);
      if (controller.signal.aborted || response === null || (object(response) && response.cancelled === true)) {
        reason = 'Pi UI dialog cancelled';
        return undefined;
      }
      const decoded = decode(response);
      reason = 'Pi UI dialog resolved';
      return decoded;
    } catch (error) {
      if (controller.signal.aborted) {
        reason = 'Pi UI dialog cancelled';
        return undefined;
      }
      throw error;
    } finally {
      clearTimeout(timer);
      opts?.signal?.removeEventListener('abort', cancel);
      context.emit({ type: 'question_closed', id, reason });
    }
  }
  const ui: ExtensionUIContext = {
    async confirm(title, message, opts) {
      return await ask('confirm', title, message, [], {}, answer => {
        if (!object(answer) || typeof answer.confirmed !== 'boolean') throw new Error('Invalid Pi confirm answer');
        return answer.confirmed;
      }, opts) ?? false;
    },
    async select(title, options, opts) {
      // The SDK's options ARE strings, with no independent IDs. Preserve them
      // verbatim as both ID and label; never infer an approval by position.
      return ask('select', title, '', options.map(value => ({ id: value, label: value })), {}, answer => {
        if (!object(answer) || typeof answer.option_id !== 'string' || !options.includes(answer.option_id)) {
          throw new Error('Invalid Pi selection answer');
        }
        return answer.option_id;
      }, opts);
    },
    async input(title, placeholder, opts) {
      return ask('input', title, '', [], { initial_value: '', placeholder: placeholder ?? '' }, answer => {
        if (!object(answer) || typeof answer.text !== 'string') throw new Error('Invalid Pi input answer');
        return answer.text;
      }, opts);
    },
    async editor(title, prefill) {
      return ask('editor', title, '', [], { initial_value: prefill ?? '' }, answer => {
        if (!object(answer) || typeof answer.text !== 'string') throw new Error('Invalid Pi editor answer');
        return answer.text;
      });
    },
    notify: notice,
    setStatus: (key, text) => notice(`Pi status ${key}: ${text ?? '(cleared)'}`),
    setWorkingMessage: message => notice(message ?? 'Pi working message cleared.'),
    setWorkingVisible: () => unsupported('setWorkingVisible'),
    setWorkingIndicator: () => unsupported('setWorkingIndicator'),
    setHiddenThinkingLabel: () => unsupported('setHiddenThinkingLabel'),
    setWidget: (key, content) => {
      if (typeof content === 'function') unsupported('component widget');
      notice(`Pi widget ${key}: ${content?.join('\n') ?? '(cleared)'}`);
    },
    onTerminalInput: () => unsupported('onTerminalInput'),
    setFooter: () => unsupported('setFooter'),
    setHeader: () => unsupported('setHeader'),
    setTitle: title => notice(`Pi title: ${title}`),
    custom: async () => unsupported('custom component'),
    pasteToEditor: () => unsupported('pasteToEditor'),
    setEditorText: () => unsupported('setEditorText'),
    getEditorText: () => unsupported('getEditorText'),
    addAutocompleteProvider: () => unsupported('addAutocompleteProvider'),
    setEditorComponent: () => unsupported('setEditorComponent'),
    getEditorComponent: () => unsupported('getEditorComponent'),
    get theme() { return unsupported('theme'); },
    getAllThemes: () => unsupported('getAllThemes'),
    getTheme: () => unsupported('getTheme'),
    setTheme: () => ({ success: false, error: 'Pi native UI does not render terminal themes' }),
    getToolsExpanded: () => unsupported('getToolsExpanded'),
    setToolsExpanded: () => unsupported('setToolsExpanded'),
  };
  return ui;
}

// A structural slice of the real SDK interface keeps provider behavior testable
// without credentials, loading user extensions, spawning tools or making calls.
export interface PiSession {
  sessionManager: Pick<SessionManager, 'getSessionId' | 'getLeafEntry'>;
  subscribe: AgentSession['subscribe'];
  prompt: AgentSession['prompt'];
  abort: AgentSession['abort'];
  dispose: AgentSession['dispose'];
}

export function attachPiSession(context: AdapterContext, session: PiSession): NativeAdapter {
  const capabilities = { prompt: true, interrupt: true, questions: true, follow_up: true };
  let closed = false;
  let turnId: string | undefined;
  const tools = new Map<string, { name: string; summary: string }>();
  const pending = new Set<Promise<void>>();
  const mode = transcriptMode(context.env.PHUX_AGENT_TRANSCRIPT);
  let providerSessionId = session.sessionManager.getSessionId();
  context.identity(providerSessionId);
  const identity = (): void => {
    const actual = session.sessionManager.getSessionId();
    if (actual !== providerSessionId) {
      providerSessionId = actual;
      context.identity(actual);
      turnId = undefined;
      context.turn(undefined);
      context.closeQuestions('Pi session changed');
    }
  };
  const state = (value: 'running' | 'idle' | 'stopped'): void => {
    context.emit({ type: 'state', state: value === 'running' ? 'working' : 'done',
      ...(turnId === undefined ? {} : { turn_id: turnId }) });
  };
  const transcript = (entry: TranscriptEntry): void => {
    if (mode !== 'off') context.emit({ type: 'transcript', entry: transcriptData(context.provider, entry).entry });
  };
  function message(event: Extract<AgentSessionEvent, { type: 'message_start' | 'message_update' | 'message_end' }>, final: boolean): void {
    if (closed) return;
    identity();
    const msg = event.message;
    // message_end is emitted BEFORE synchronous persistence in 1.1.0. Its
    // microtask runs after appendMessage, so getLeafEntry exposes the real ID.
    const leaf = final ? session.sessionManager.getLeafEntry() : undefined;
    const persistedId = leaf?.type === 'message' && leaf.message === msg ? leaf.id : undefined;
    const id = msg.role === 'assistant' ? msg.responseId ?? persistedId : persistedId;
    if (msg.role === 'user' && persistedId !== undefined) {
      turnId = persistedId; // Pi exposes no separate turn ID; use its actual input entry ID.
      context.turn(turnId);
      state('running');
    }
    if (!id) return; // Never synthesize an upstream message identity.
    if (msg.role === 'assistant') {
      const text = msg.content.filter(block => block.type === 'text').map(block => block.text).join('\n');
      const thinking = msg.content.filter(block => block.type === 'thinking').map(block => block.thinking).join('\n');
      if (text || final) transcript({ id, role: 'assistant', text, final, truncated: false });
      if (thinking) transcript({ id: `${id}:thinking`, role: 'thinking', text: thinking, final, truncated: false });
      if (final && msg.stopReason === 'error') context.emit({
        type: 'notice', level: 'error', text: 'Pi assistant response failed.',
      });
    } else if (msg.role === 'user') {
      const text = typeof msg.content === 'string' ? msg.content : msg.content.filter(block => block.type === 'text').map(block => block.text).join('\n');
      transcript({ id, role: 'user', text, final, truncated: false });
    }
    // Do not publish system prompts, opaque custom details, auth state or raw messages.
  }
  const unsubscribe = session.subscribe(event => {
    if (closed) return;
    identity();
    switch (event.type) {
      case 'agent_start': state('running'); break;
      case 'agent_settled':
        context.closeQuestions(event.aborted ? 'Pi run aborted' : 'Pi run settled');
        turnId = undefined;
        context.turn(undefined);
        state(event.aborted ? 'stopped' : 'idle');
        tools.clear();
        break;
      case 'message_start': case 'message_update': message(event, false); break;
      case 'message_end': queueMicrotask(() => message(event, true)); break;
      case 'tool_execution_start': {
        const tool = { name: event.toolName, summary: summarizeArgs(event.args) };
        tools.set(event.toolCallId, tool);
        transcript({ id: event.toolCallId, role: 'tool', text: '', final: false, truncated: false,
          tool: { ...tool, call_id: event.toolCallId, status: 'running', output: '' } });
        break;
      }
      case 'tool_execution_update': case 'tool_execution_end': {
        const tool = tools.get(event.toolCallId) ?? { name: event.toolName, summary: '' };
        const result: unknown = event.type === 'tool_execution_end' ? event.result : event.partialResult;
        const output = mode === 'full' && object(result) && Array.isArray(result.content)
          ? result.content.filter((block: unknown): block is { type: 'text'; text: string } =>
            object(block) && block.type === 'text' && typeof block.text === 'string').map(block => block.text).join('\n') : '';
        transcript({ id: event.toolCallId, role: 'tool', text: '', final: event.type === 'tool_execution_end', truncated: false,
          tool: { ...tool, call_id: event.toolCallId,
            status: event.type === 'tool_execution_end' ? event.isError ? 'error' : 'ok' : 'running', output } });
        if (event.type === 'tool_execution_end') tools.delete(event.toolCallId);
        break;
      }
    }
  });
  state('idle');
  return {
    capabilities,
    get providerSessionId() { return providerSessionId; },
    async prompt(text, operationId) {
      if (closed) return { disposition: 'refused', detail: 'Pi session is closed.' };
      // Never await this promise here: extension input/command preflight and the
      // entire run can await a brokered answer. The command dispatcher must drain.
      const task = session.prompt(text, { streamingBehavior: 'followUp', source: 'interactive',
        preflightResult: disposition => context.emit({ type: 'notice', level: 'info',
          text: `Pi operation ${operationId}: SDK preflight ${disposition}.` }),
      });
      const observed = task.then(() => { identity(); }, () => {
        if (!closed) {
          context.emit({ type: 'notice', level: 'error', text: `Pi operation ${operationId} failed in the SDK.` });
        }
      });
      pending.add(observed);
      void observed.finally(() => pending.delete(observed));
      return { disposition: 'queued', detail: 'Pi SDK prompt invoked; preflight pending, not provider admission.' };
    },
    async interrupt(requestedTurnId) {
      if (closed || turnId === undefined || requestedTurnId !== turnId) {
        return { disposition: 'refused', detail: 'Pi interrupt does not match the active input entry.' };
      }
      context.closeQuestions('Pi interruption requested');
      await session.abort();
      return { disposition: 'completed', detail: 'Pi SDK abort completed.' };
    },
    async close() {
      if (closed) return;
      closed = true;
      context.closeQuestions('Pi session closed');
      context.connection(false);
      try {
        await session.abort();
        await Promise.all(pending);
      } finally {
        unsubscribe();
        session.dispose();
        context.turn(undefined);
      }
    },
  };
}
