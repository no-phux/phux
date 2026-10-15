import { spawn } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { constants } from 'node:fs';
import { access } from 'node:fs/promises';
import { isAbsolute } from 'node:path';
import type {
  CanUseTool, ElicitationResult, OnElicitation, Options, Query, SDKMessage,
  SDKPartialAssistantMessage, SDKUserMessage, query as sdkQuery,
} from '@anthropic-ai/claude-agent-sdk/core';
import {
  keepHead, keepTail, MAX_TOOL_OUTPUT_BYTES, summarizeArgs, transcriptData, transcriptMode, TranscriptGate,
} from '../../runtime/src/transcript.js';
import type { AdapterContext, Json, NativeAdapter, NativeQuestionItem, TranscriptEntry, TranscriptTool } from './types.js';

// Prerequisites: the private runtime pins SDK 0.3.300 and its peers, excludes
// optional bundled CLI packages, and has explicit Anthropic terms acceptance.
// command must name an existing Claude Code 2.1.300 native executable. Never
// install, locate a bundled fallback, or silently upgrade the user's executable.
const CLAUDE_VERSION = '2.1.300';
const MAX_PENDING = 64;
const MAX_TEXT_BYTES = 16 * 1024;

class PromptStream implements AsyncIterable<SDKUserMessage> {
  private readonly queued: SDKUserMessage[] = [];
  private waiting?: (message: IteratorResult<SDKUserMessage>) => void;
  private ended = false;

  push(message: SDKUserMessage): void {
    if (this.ended) throw new Error('Claude input is closed');
    if (this.waiting) {
      const resolve = this.waiting;
      this.waiting = undefined;
      resolve({ done: false, value: message });
    } else this.queued.push(message);
  }

  finish(): void {
    this.ended = true;
    this.queued.length = 0;
    this.waiting?.({ done: true, value: undefined });
    this.waiting = undefined;
  }

  [Symbol.asyncIterator](): AsyncIterator<SDKUserMessage> {
    return {
      next: () => {
        const message = this.queued.shift();
        if (message) return Promise.resolve({ done: false, value: message });
        if (this.ended) return Promise.resolve({ done: true, value: undefined });
        const { promise, resolve } = Promise.withResolvers<IteratorResult<SDKUserMessage>>();
        this.waiting = resolve;
        return promise;
      },
      return: async () => { this.finish(); return { done: true, value: undefined }; },
    };
  }
}

/** Do not pass host registration credentials or runtime instrumentation to tools. */
export function claudeEnvironment(source: NodeJS.ProcessEnv): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = {};
  for (const [key, value] of Object.entries(source)) {
    if (key.startsWith('PHUX_NATIVE_') || key.startsWith('PHUX_AGENT_HOST_') ||
        /(?:BINDING|DELEGAT(?:ED|ION)).*(?:PROOF|TOKEN|SECRET|CREDENTIAL)/i.test(key) ||
        key === 'NODE_OPTIONS' || key === 'NODE_PATH' || key === 'DEBUG' ||
        key === 'DEBUG_CLAUDE_AGENT_SDK' || key === 'CLAUDECODE') continue;
    env[key] = value;
  }
  env.PHUX_AGENT_REPORTING = '0';
  return env;
}

function record(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function boundedJson(value: unknown): Json {
  const encoded = JSON.stringify(value);
  if (encoded === undefined || Buffer.byteLength(encoded) > 14 * 1024) {
    throw new Error('Claude UI payload exceeds native question bounds');
  }
  return JSON.parse(encoded) as Json;
}

function textContent(content: unknown): string {
  if (typeof content === 'string') return keepTail(content, MAX_TOOL_OUTPUT_BYTES);
  if (!Array.isArray(content)) return '';
  let text = '';
  for (const block of content) {
    if (record(block) && block.type === 'text' && typeof block.text === 'string') {
      text = keepTail(text + (text ? '\n' : '') + block.text, MAX_TOOL_OUTPUT_BYTES);
    }
  }
  return text;
}

function askItems(input: Record<string, unknown>): NativeQuestionItem[] {
  if (!Array.isArray(input.questions) || input.questions.length < 1 || input.questions.length > 4) {
    throw new Error('Unsupported AskUserQuestion questions');
  }
  const items: NativeQuestionItem[] = input.questions.map((item: unknown) => {
    if (!record(item) || typeof item.question !== 'string' || typeof item.header !== 'string' ||
        typeof item.multiSelect !== 'boolean' || !Array.isArray(item.options) ||
        item.options.length < 2 || item.options.length > 4) {
      throw new Error('Unsupported AskUserQuestion item');
    }
    const options = item.options.map((option: unknown) => {
      if (!record(option) || typeof option.label !== 'string' || typeof option.description !== 'string' ||
          option.preview !== undefined) {
        throw new Error('AskUserQuestion previews require unsupported provider UI');
      }
      // Claude has no opaque item/option IDs: the answer object is keyed by
      // exact question text and its values are exact labels, not array indices.
      return { id: option.label, label: option.label, description: option.description };
    });
    if (new Set(options.map((option) => option.id)).size !== options.length) {
      throw new Error('Ambiguous AskUserQuestion labels');
    }
    return { id: item.question, question: item.question, header: item.header, options, multi: item.multiSelect };
  });
  if (new Set(items.map((item) => item.id)).size !== items.length) throw new Error('Ambiguous AskUserQuestion keys');
  return items;
}

function askAnswers(items: NativeQuestionItem[], response: Json): Record<string, string> {
  if (!record(response) || !Array.isArray(response.answers) || response.answers.length !== items.length) {
    throw new Error('AskUserQuestion requires every answer');
  }
  const answers: Record<string, string> = Object.create(null) as Record<string, string>;
  for (const answer of response.answers) {
    if (!record(answer) || typeof answer.id !== 'string' || Object.hasOwn(answers, answer.id)) {
      throw new Error('Invalid AskUserQuestion answer identity');
    }
    const item = items.find((candidate) => candidate.id === answer.id);
    if (!item || !Array.isArray(answer.selected_options) ||
        !answer.selected_options.every((id: unknown) => typeof id === 'string' && item.options.some((option) => option.id === id)) ||
        new Set(answer.selected_options).size !== answer.selected_options.length ||
        (!item.multi && answer.selected_options.length > 1) ||
        (answer.custom_input !== undefined && typeof answer.custom_input !== 'string')) {
      throw new Error('Invalid AskUserQuestion option selection');
    }
    const selected = answer.selected_options as string[];
    const custom = typeof answer.custom_input === 'string' ? answer.custom_input : '';
    if (selected.length === 0 && custom.trim() === '') throw new Error('Empty AskUserQuestion answer');
    if (!item.multi && selected.length > 0 && custom !== '') throw new Error('Ambiguous single-choice answer');
    answers[answer.id] = [...selected, ...(custom ? [custom] : [])].join(', ');
  }
  return answers;
}

interface StreamMessage {
  id: string;
  blocks: Map<number, { role: 'assistant' | 'thinking'; text: string; truncated: boolean }>;
}

/** Projects SDK content blocks into the existing bounded transcript convention. */
class Transcript {
  private readonly gate = new TranscriptGate();
  private readonly streams = new Map<string, StreamMessage>();
  private readonly streamed = new Set<string>();
  private readonly tools = new Map<string, TranscriptTool>();
  private readonly mode;

  constructor(private readonly context: AdapterContext) {
    this.mode = transcriptMode(context.env.PHUX_AGENT_TRANSCRIPT);
  }

  emit(entry: TranscriptEntry): void {
    if (this.mode === 'off') return;
    const normalized = transcriptData('claude', entry).entry;
    if (entry.role === 'tool' || this.gate.admit(normalized)) this.context.emit({ type: 'transcript', entry: normalized });
  }

  private tool(id: string, name: string, summary: string, status: TranscriptTool['status'], output = ''): void {
    const row: TranscriptTool = { name, call_id: id, summary, status, output: this.mode === 'full' ? output : '' };
    const previous = this.tools.get(id);
    if (previous?.name === name && previous.summary === row.summary && previous.status === status && previous.output === row.output) return;
    if (this.tools.size >= 512 && !this.tools.has(id)) this.tools.delete(this.tools.keys().next().value!);
    this.tools.set(id, row);
    this.emit({ id, role: 'tool', text: '', truncated: false, final: status !== 'running', tool: row });
  }

  private stream(message: SDKPartialAssistantMessage): void {
    const key = message.parent_tool_use_id ?? '';
    const event = message.event;
    if (event.type === 'message_start') {
      if (this.streams.size >= 64 && !this.streams.has(key)) throw new Error('Too many Claude content streams');
      this.streams.set(key, { id: event.message.id, blocks: new Map() });
      this.streamed.add(event.message.id);
      if (this.streamed.size > 512) this.streamed.delete(this.streamed.values().next().value!);
      return;
    }
    const stream = this.streams.get(key);
    if (!stream) return;
    if (event.type === 'content_block_start') {
      const block = event.content_block;
      if (block.type === 'text' || block.type === 'thinking') {
        if (stream.blocks.size >= 256 && !stream.blocks.has(event.index)) throw new Error('Too many Claude content blocks');
        const original = block.type === 'text' ? block.text : block.thinking;
        const text = keepHead(original, MAX_TEXT_BYTES);
        stream.blocks.set(event.index, {
          role: block.type === 'text' ? 'assistant' : 'thinking', text, truncated: original.length > text.length,
        });
      } else if (block.type === 'tool_use' || block.type === 'server_tool_use') {
        this.tool(block.id, block.name, summarizeArgs(block.input), 'running');
      }
    } else if (event.type === 'content_block_delta') {
      const block = stream.blocks.get(event.index);
      const delta = event.delta;
      if (block && (delta.type === 'text_delta' || delta.type === 'thinking_delta')) {
        const combined = block.text + (delta.type === 'text_delta' ? delta.text : delta.thinking);
        block.text = keepHead(combined, MAX_TEXT_BYTES);
        block.truncated ||= combined.length > block.text.length;
        this.emit({ id: `${stream.id}:${event.index}`, role: block.role, text: block.text, truncated: block.truncated, final: false });
      }
    } else if (event.type === 'content_block_stop') {
      const block = stream.blocks.get(event.index);
      if (block) this.emit({ id: `${stream.id}:${event.index}`, role: block.role, text: block.text, truncated: block.truncated, final: true });
    } else if (event.type === 'message_stop') {
      for (const [index, block] of stream.blocks) {
        this.emit({ id: `${stream.id}:${index}`, role: block.role, text: block.text, truncated: block.truncated, final: true });
      }
      this.streams.delete(key);
    }
  }

  message(message: SDKMessage): void {
    if (message.type === 'stream_event') this.stream(message);
    else if (message.type === 'assistant') {
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
        if (block.type === 'text' || block.type === 'thinking') {
          if (!this.streamed.has(message.message.id)) {
            this.emit({ id: `${message.uuid}:${index}`, role: block.type === 'text' ? 'assistant' : 'thinking',
              text: block.type === 'text' ? block.text : block.thinking, truncated: message.aborted === true, final: true });
          }
        } else if (block.type === 'tool_use' || block.type === 'server_tool_use') {
          this.tool(block.id, block.name, summarizeArgs(block.input), 'running');
        }
      }
      if (message.error) this.context.emit({ type: 'notice', level: 'error', text: `Claude assistant error: ${message.error}` });
    } else if (message.type === 'user') {
      if (typeof message.message.content !== 'string') {
        for (const block of message.message.content) {
          if (block.type === 'tool_result') {
            const previous = this.tools.get(block.tool_use_id);
            this.tool(block.tool_use_id, previous?.name ?? 'Tool', previous?.summary ?? '',
              block.is_error ? 'error' : 'ok', textContent(block.content));
          }
        }
      }
      // User prompts are emitted on local queueing, not duplicated on CLI echoes.
    } else if (message.type === 'tool_progress') {
      const previous = this.tools.get(message.tool_use_id);
      this.tool(message.tool_use_id, message.tool_name, previous?.summary ?? '', 'running');
    } else if (message.type === 'system' && message.subtype === 'local_command_output') {
      this.emit({ id: message.uuid, role: 'system', text: message.content, truncated: false, final: true });
    }
  }
}

/** The optional factory is the real SDK seam used by provider-module tests. */
export async function createAdapter(context: AdapterContext, startQuery?: typeof sdkQuery): Promise<NativeAdapter> {
  if (context.command.length !== 1 || !isAbsolute(context.command[0]!)) {
    throw new Error('Claude native mode requires one absolute existing native executable path, without CLI flags');
  }
  const executable = context.command[0]!;
  if (/\.(?:[cm]?js|tsx?|jsx)$/i.test(executable)) throw new Error('Claude native mode requires the compatible native executable, not a JS CLI entry');
  await access(executable, constants.X_OK);
  // SDK import must follow isolated-runtime resolution hook and sanitized process environment.
  const [{ query }, { AjvJsonSchemaValidator }, protocol, { default: Ajv }, { default: addFormats }] = await Promise.all([
    import('@anthropic-ai/claude-agent-sdk/core'), import('@modelcontextprotocol/sdk/validation/ajv'),
    import('@modelcontextprotocol/sdk/types.js'), import('ajv'), import('ajv-formats'),
  ]);
  const ajv = new Ajv({ strict: true, allErrors: true, validateFormats: true });
  addFormats(ajv);
  // enumNames is the MCP form's display-label annotation, not a validation rule.
  ajv.addKeyword({ keyword: 'enumNames', schemaType: 'array', valid: true });
  const validator = new AjvJsonSchemaValidator(ajv);
  const shutdown = new AbortController();
  const input = new PromptStream();
  const transcript = new Transcript(context);
  const pending = new Map<string, string>();
  const questions = new Map<string, { toolId?: string; submitted: boolean; signal: AbortSignal; abort: () => void }>();
  let currentTurn: string | undefined;
  let closed = false;
  let exited: Promise<void> | undefined;

  const notice = (level: 'info' | 'warning' | 'error', text: string): void => context.emit({ type: 'notice', level, text });
  const closeQuestion = (id: string, reason: string): void => {
    const question = questions.get(id);
    if (!question) return;
    questions.delete(id);
    question.signal.removeEventListener('abort', question.abort);
    context.emit({ type: 'question_closed', id, reason });
  };
  const openQuestion = (id: string, signal: AbortSignal, toolId?: string): void => {
    if (questions.size >= MAX_PENDING || questions.has(id)) throw new Error('Claude question already pending or question limit exceeded');
    const abort = () => closeQuestion(id, 'Claude SDK cancelled request');
    questions.set(id, { toolId, submitted: false, signal, abort });
    signal.addEventListener('abort', abort, { once: true });
  };
  const submitted = (id: string): void => {
    const question = questions.get(id);
    if (question) question.submitted = true;
  };
  const canUseTool: CanUseTool = async (toolName, toolInput, options) => {
    const deny = (message: string) => ({ behavior: 'deny' as const, message, toolUseID: options.toolUseID });
    const signal = AbortSignal.any([options.signal, shutdown.signal]);
    if (signal.aborted) return deny('Claude permission request cancelled');
    // 0.3.300 forwards this provider UI flag in core.mjs even though its public
    // CanUseTool declaration omits it. A permission click cannot replace the
    // unknown tool's required interactive renderer.
    const providerOptions: unknown = options;
    if (record(providerOptions) && providerOptions.requiresUserInteraction === true && toolName !== 'AskUserQuestion') {
      notice('warning', 'Claude tool requires unsupported native provider UI; denied');
      return deny('Unsupported provider UI');
    }
    try {
      context.emit({ type: 'state', state: 'blocked', turn_id: currentTurn });
      if (toolName === 'AskUserQuestion') {
        const items = askItems(toolInput);
        openQuestion(options.requestId, signal, options.toolUseID);
        const response = await context.question({ id: options.requestId, kind: 'ask', title: options.title ?? 'Claude question',
          message: options.description ?? '', options: [], questions: items,
          fields: { tool_use_id: options.toolUseID, request_id: options.requestId } }, signal);
        submitted(options.requestId);
        if (signal.aborted || (record(response) && response.cancelled === true)) return deny('Claude question cancelled');
        return { behavior: 'allow', toolUseID: options.toolUseID, updatedInput: { ...toolInput, answers: askAnswers(items, response) } };
      }
      openQuestion(options.requestId, signal, options.toolUseID);
      const response = await context.question({ id: options.requestId, kind: 'permission',
        title: options.title ?? options.displayName ?? toolName,
        message: options.description ?? options.decisionReason ?? summarizeArgs(toolInput),
        // These are the official PermissionResult.behavior values. No inferred
        // default, persistent permission rule, or input rewrite is offered.
        options: [{ id: 'deny', label: 'Deny' }, { id: 'allow', label: 'Allow this tool call' }], questions: [],
        fields: boundedJson({ request_id: options.requestId, tool_use_id: options.toolUseID, tool_name: toolName,
          blocked_path: options.blockedPath, decision_reason: options.decisionReason,
          default_to_no: options.defaultToNo, suppress_always_allow_rule: options.suppressAlwaysAllowRule }) }, signal);
      submitted(options.requestId);
      if (signal.aborted || (record(response) && response.cancelled === true)) return deny('Claude permission request cancelled');
      if (record(response) && response.option_id === 'allow') return { behavior: 'allow', toolUseID: options.toolUseID };
      if (record(response) && response.option_id === 'deny') return deny('User denied this Claude tool call');
      return deny('Invalid native Claude permission answer');
    } catch {
      submitted(options.requestId);
      notice('warning', 'Claude tool request denied: cancelled or unsupported native UI/answer');
      return deny('Native UI could not authorize this tool call');
    }
  };
  const onElicitation: OnElicitation = async (request, options): Promise<ElicitationResult> => {
    const signal = AbortSignal.any([options.signal, shutdown.signal]);
    if (signal.aborted) return { action: 'cancel' };
    if (request.mode === 'url') {
      // URL authentication needs provider browser completion, not a fake form
      // approval. Native hosts currently cannot implement that flow.
      notice('warning', 'Claude MCP URL elicitation is unsupported; request cancelled');
      return { action: 'cancel' };
    }
    try {
      const parsed = protocol.ElicitRequestFormParamsSchema.safeParse({ mode: 'form', message: request.message, requestedSchema: request.requestedSchema });
      if (!parsed.success || !request.requestedSchema) throw new Error('Unsupported MCP form');
      // Validate the ORIGINAL schema, not Zod's stripped schema: unsupported
      // constraints must never silently disappear before authorization.
      const validate = validator.getValidator(request.requestedSchema as typeof parsed.data.requestedSchema);
      context.emit({ type: 'state', state: 'blocked', turn_id: currentTurn });
      openQuestion(options.requestId, signal);
      const response = await context.question({ id: options.requestId, kind: 'form', title: request.title ?? request.serverName,
        message: request.message, options: [], questions: [], fields: boundedJson(request.requestedSchema) }, signal);
      submitted(options.requestId);
      if (signal.aborted || (record(response) && response.cancelled === true)) return { action: 'cancel' };
      if (!record(response) || !record(response.values) || !validate(response.values).valid) {
        notice('warning', 'Claude MCP form answer failed provider schema validation');
        return { action: 'decline' };
      }
      return protocol.ElicitResultSchema.parse({ action: 'accept', content: response.values });
    } catch {
      submitted(options.requestId);
      notice('warning', 'Claude MCP elicitation cancelled: unsupported schema, UI, or answer');
      return { action: 'cancel' };
    }
  };

  const options: Options = {
    pathToClaudeCodeExecutable: executable, cwd: context.cwd, env: claudeEnvironment(context.env),
    ...(context.resumeSessionId ? { resume: context.resumeSessionId } : {}),
    abortController: shutdown, permissionMode: 'default', includePartialMessages: true, canUseTool, onElicitation,
    // No terminal-only dialog kinds are advertised. The SDK fails closed on
    // absence; an unexpected delivery is explicitly cancelled, never approved.
    supportedDialogKinds: [], onUserDialog: async () => ({ behavior: 'cancelled' }),
    // This release's sdk.d.ts references but does not declare SpawnOptions;
    // keep the concrete published spawn seam typed instead of inheriting it.
    spawnClaudeCodeProcess: (spawnOptions: { command: string; args: string[]; cwd?: string; env: Record<string, string | undefined>; signal: AbortSignal }) => {
      if (spawnOptions.command !== executable) throw new Error('Claude SDK attempted a different executable');
      const child = spawn(executable, spawnOptions.args, { cwd: spawnOptions.cwd,
        env: claudeEnvironment(spawnOptions.env), signal: spawnOptions.signal,
        detached: false, windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'] });
      // Always drain stderr, but never dump credentials, account/state, or raw
      // subprocess diagnostics onto the bridge's protocol stream.
      child.stderr.resume();
      const { promise, resolve } = Promise.withResolvers<void>();
      child.once('close', () => resolve());
      child.once('error', () => resolve());
      exited = promise;
      return child;
    },
  };
  const conversation: Query = (startQuery ?? query)({ prompt: input, options });
  const adapter: NativeAdapter = {
    capabilities: { prompt: true, interrupt: true, questions: true, follow_up: true },
    async prompt(text, operationId) {
      if (closed) return { disposition: 'refused', detail: 'Claude query is closed' };
      if (pending.size >= MAX_PENDING) return { disposition: 'refused', detail: 'Claude has 64 unconsumed prompts' };
      const uuid = randomUUID();
      pending.set(uuid, operationId);
      input.push({ type: 'user', uuid, message: { role: 'user', content: text }, parent_tool_use_id: null,
        origin: { kind: 'human' }, ...(adapter.providerSessionId ? { session_id: adapter.providerSessionId } : {}) });
      transcript.emit({ id: uuid, role: 'user', text, truncated: false, final: true });
      return { disposition: 'queued', detail: `Queued in Claude SDK streaming input (${uuid}); provider consumption not yet confirmed` };
    },
    async interrupt(turnId) {
      if (closed || !currentTurn || currentTurn !== turnId) return { disposition: 'refused', detail: 'Claude turn is no longer current' };
      try {
        const receipt = await conversation.interrupt();
        context.closeQuestions('Claude acknowledged interrupt');
        return { disposition: 'completed', detail: receipt
          ? `Claude acknowledged interrupt; ${receipt.still_queued.length} queued prompts remain. Turn termination is reported separately.`
          : 'Claude acknowledged interrupt; turn termination is reported separately' };
      } catch {
        return { disposition: 'unknown', detail: 'Claude interrupt outcome is unknown; it was not resent' };
      }
    },
    async close() {
      if (!closed) {
        closed = true;
        input.finish();
        shutdown.abort();
        context.closeQuestions('Claude query closed');
        conversation.close();
      }
      await draining;
      await exited;
    },
  };

  // SDK 0.3.300 dispatches callback promises independently of readMessages;
  // awaiting a brokered question never parks this event drain or command input.
  const draining = (async () => {
    try {
      for await (const message of conversation) {
        if (message.session_id && message.session_id !== adapter.providerSessionId) {
          adapter.providerSessionId = message.session_id;
          context.identity(message.session_id);
        }
        const outcomeToolIds: string[] = [];
        if (message.type === 'tool_progress') outcomeToolIds.push(message.tool_use_id);
        else if (message.type === 'system' && message.subtype === 'permission_denied') outcomeToolIds.push(message.tool_use_id);
        else if (message.type === 'user' && typeof message.message.content !== 'string') {
          for (const block of message.message.content) if (block.type === 'tool_result') outcomeToolIds.push(block.tool_use_id);
        }
        for (const [id, question] of questions) {
          if (!question.submitted) continue;
          if (question.toolId && outcomeToolIds.includes(question.toolId)) {
            closeQuestion(id, 'Claude SDK reported the requested tool outcome');
          } else if (!question.toolId && (message.type === 'assistant' || message.type === 'stream_event' || message.type === 'tool_progress')) {
            // Form callbacks have no consumption acknowledgment in this SDK.
            closeQuestion(id, 'Claude SDK continued after submitted elicitation');
          } else if (message.type === 'result') {
            closeQuestion(id, 'Claude SDK turn ended after submitted response');
          }
        }
        if ((message.type === 'assistant' || message.type === 'stream_event') && message.parent_tool_use_id === null) {
          if (message.user_message_uuid) {
            // A UUID allocated on submission becomes a turn identity ONLY when
            // the provider echoes actual consumption, never on local queueing.
            currentTurn = message.user_message_uuid;
            context.turn(currentTurn);
          }
          for (const id of message.user_message_uuids ?? (message.user_message_uuid ? [message.user_message_uuid] : [])) {
            const operation = pending.get(id);
            if (operation !== undefined) { pending.delete(id); notice('info', `Claude consumed prompt operation ${operation} (${id})`); }
          }
          let waitingForAnswer = false;
          for (const question of questions.values()) {
            if (!question.submitted) { waitingForAnswer = true; break; }
          }
          context.emit({ type: 'state', state: waitingForAnswer ? 'blocked' : 'working', turn_id: currentTurn });
        }
        transcript.message(message);
        if (message.type === 'result') {
          for (const id of message.user_message_uuids ?? (message.user_message_uuid ? [message.user_message_uuid] : [])) pending.delete(id);
          if (message.is_error) notice('error', `Claude turn ended with ${message.subtype}`);
          currentTurn = undefined;
          context.turn(undefined);
          context.emit({ type: 'state', state: 'done' });
          // Result ends ONE turn, not the input iterator or provider session.
        } else if (message.type === 'system' && message.subtype === 'session_state_changed') {
          if (message.state === 'idle') { currentTurn = undefined; context.turn(undefined); }
          context.emit({ type: 'state', state: message.state === 'idle' ? 'done' : message.state === 'requires_action' ? 'blocked' : 'working', turn_id: currentTurn });
        }
      }
      if (!closed) notice('warning', 'Claude SDK stream ended; native control is closed');
    } catch {
      if (!closed) notice('error', 'Claude SDK stream failed; native control is closed');
    } finally {
      closed = true;
      context.connection(false);
      input.finish();
      pending.clear();
      shutdown.abort();
      context.closeQuestions('Claude SDK stream closed');
      context.turn(undefined);
      currentTurn = undefined;
      context.emit({ type: 'state', state: 'done' });
    }
  })();
  try {
    const initialized = await conversation.initializationResult();
    if (initialized.claude_code_version !== CLAUDE_VERSION) {
      throw new Error(`Claude SDK 0.3.300 requires a verified Claude Code ${CLAUDE_VERSION} executable`);
    }
    if (closed) throw new Error('Claude query ended during initialization');
    context.connection(true);
    return adapter;
  } catch (error) {
    await adapter.close();
    throw error;
  }
}
