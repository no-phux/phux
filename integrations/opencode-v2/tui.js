// @bun
// src/tui.ts
import { Plugin } from "@opencode/plugin/tui";
import { createEffect } from "solid-js";

// ../runtime/src/errors.ts
class PhuxError extends Error {
  code;
  argv;
  exitCode;
  stderr;
  cliError;
  constructor(code, message, details = {}) {
    super(message, details.cause === undefined ? undefined : { cause: details.cause });
    this.name = "PhuxError";
    this.code = code;
    this.argv = details.argv;
    this.exitCode = details.exitCode;
    this.stderr = details.stderr;
    this.cliError = details.cliError;
  }
}

// ../runtime/src/runner.ts
import { spawn } from "child_process";
var DEFAULT_MAX_OUTPUT_BYTES = 4 * 1024 * 1024;
var nodeProcessRunner = (request) => new Promise((resolve, reject) => {
  if (request.signal?.aborted) {
    resolve({ termination: "aborted", exitCode: null, stdout: "", stderr: "" });
    return;
  }
  validateNonNegativeFinite(request.timeoutMs, "timeoutMs");
  const maxStdoutBytes = outputLimit(request.maxStdoutBytes, "maxStdoutBytes");
  const maxStderrBytes = outputLimit(request.maxStderrBytes, "maxStderrBytes");
  const child = spawn(request.executable, [...request.args], {
    cwd: request.cwd,
    env: request.env,
    shell: false,
    detached: true,
    stdio: ["ignore", "pipe", "pipe"]
  });
  const stdoutChunks = [];
  const stderrChunks = [];
  let stdoutBytes = 0;
  let stderrBytes = 0;
  let termination = "completed";
  let limitedStream;
  let timeout;
  let forceKill;
  const stop = (reason, stream) => {
    if (termination !== "completed")
      return;
    termination = reason;
    limitedStream = stream;
    killProcessGroup(child, "SIGTERM");
    forceKill = setTimeout(() => killProcessGroup(child, "SIGKILL"), 1000);
    forceKill.unref();
  };
  child.stdout.on("data", (chunk) => {
    if (termination !== "completed")
      return;
    const remaining = maxStdoutBytes - stdoutBytes;
    if (chunk.length > remaining) {
      if (remaining > 0)
        stdoutChunks.push(chunk.subarray(0, remaining));
      stdoutBytes = maxStdoutBytes;
      stop("output_limit", "stdout");
      return;
    }
    stdoutChunks.push(chunk);
    stdoutBytes += chunk.length;
  });
  child.stderr.on("data", (chunk) => {
    if (termination !== "completed")
      return;
    const remaining = maxStderrBytes - stderrBytes;
    if (chunk.length > remaining) {
      if (remaining > 0)
        stderrChunks.push(chunk.subarray(0, remaining));
      stderrBytes = maxStderrBytes;
      stop("output_limit", "stderr");
      return;
    }
    stderrChunks.push(chunk);
    stderrBytes += chunk.length;
  });
  const onAbort = () => stop("aborted");
  request.signal?.addEventListener("abort", onAbort, { once: true });
  if (request.timeoutMs !== undefined) {
    timeout = setTimeout(() => stop("timed_out"), request.timeoutMs);
    timeout.unref();
  }
  child.once("error", (error) => {
    cleanup();
    if (forceKill !== undefined)
      clearTimeout(forceKill);
    reject(error);
  });
  child.once("close", (exitCode) => {
    cleanup();
    if (forceKill !== undefined && !processGroupExists(child.pid))
      clearTimeout(forceKill);
    const base = {
      exitCode,
      stdout: Buffer.concat(stdoutChunks, stdoutBytes).toString("utf8"),
      stderr: Buffer.concat(stderrChunks, stderrBytes).toString("utf8")
    };
    if (termination === "output_limit") {
      resolve({ ...base, termination, outputLimit: limitedStream ?? "stdout" });
    } else {
      resolve({ ...base, termination });
    }
  });
  function cleanup() {
    if (timeout !== undefined)
      clearTimeout(timeout);
    request.signal?.removeEventListener("abort", onAbort);
  }
});
function outputLimit(value, name) {
  const resolved = value ?? DEFAULT_MAX_OUTPUT_BYTES;
  if (!Number.isSafeInteger(resolved) || resolved < 0) {
    throw new RangeError(`${name} must be a non-negative safe integer`);
  }
  return resolved;
}
function validateNonNegativeFinite(value, name) {
  if (value !== undefined && (!Number.isFinite(value) || value < 0)) {
    throw new RangeError(`${name} must be a non-negative finite number`);
  }
}
function killProcessGroup(child, signal) {
  if (child.pid !== undefined) {
    try {
      process.kill(-child.pid, signal);
      return;
    } catch (error) {
      if (isNoSuchProcess(error))
        return;
    }
  }
  child.kill(signal);
}
function processGroupExists(pid) {
  if (pid === undefined)
    return false;
  try {
    process.kill(-pid, 0);
    return true;
  } catch (error) {
    return !isNoSuchProcess(error);
  }
}
function isNoSuchProcess(error) {
  return error instanceof Error && "code" in error && error.code === "ESRCH";
}

// ../runtime/src/schemas.ts
var AGENT_EVENT_TYPES = [
  "session_start",
  "prompt",
  "tool_start",
  "tool_end",
  "notification",
  "ask",
  "stop",
  "session_end",
  "state",
  "provider_raw"
];

class SchemaValidationError extends Error {
  path;
  constructor(path, expectation) {
    super(`${path} must be ${expectation}`);
    this.path = path;
    this.name = "SchemaValidationError";
  }
}
function record(value, path) {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new SchemaValidationError(path, "an object");
  }
  return value;
}
function string(value, path) {
  if (typeof value !== "string")
    throw new SchemaValidationError(path, "a string");
  return value;
}
function boolean(value, path) {
  if (typeof value !== "boolean")
    throw new SchemaValidationError(path, "a boolean");
  return value;
}
function nullableString(value, path) {
  return value === null ? null : string(value, path);
}
function numberInRange(value, path, min, max) {
  if (typeof value !== "number" || !Number.isFinite(value) || value < min || value > max) {
    throw new SchemaValidationError(path, `a number from ${min} through ${max}`);
  }
  return value;
}
function oneOf(value, path, values) {
  if (typeof value !== "string" || !values.includes(value)) {
    throw new SchemaValidationError(path, values.map((item) => JSON.stringify(item)).join(", "));
  }
  return value;
}
function integer(value, path, min, max = Number.MAX_SAFE_INTEGER) {
  if (!Number.isSafeInteger(value) || value < min || value > max) {
    throw new SchemaValidationError(path, `an integer from ${min} through ${max}`);
  }
  return value;
}
function strings(value, path) {
  if (!Array.isArray(value))
    throw new SchemaValidationError(path, "an array of strings");
  return value.map((item, index) => string(item, `${path}[${index}]`));
}
function parseSessionList(value) {
  const root = record(value, "$ (phux ls --json CLI shape)");
  if (root.schema_version !== 1 && root.schema_version !== 2) {
    throw new SchemaValidationError("$.schema_version", "a supported value (1 or 2)");
  }
  const schema = root.schema_version;
  if (!Array.isArray(root.sessions)) {
    throw new SchemaValidationError("$.sessions", "an array");
  }
  const sessions = root.sessions.map((item, index) => {
    const row = record(item, `$.sessions[${index}]`);
    const name = string(row.name, `$.sessions[${index}].name`);
    if (name.length === 0)
      throw new SchemaValidationError(`$.sessions[${index}].name`, "non-empty");
    return {
      name,
      windows: integer(row.windows, `$.sessions[${index}].windows`, 0),
      attached: boolean(row.attached, `$.sessions[${index}].attached`)
    };
  });
  const terminals = schema === 1 && root.terminals === undefined ? [] : strings(root.terminals, "$.terminals");
  return { schema_version: schema, sessions, terminals };
}
function parseColor(value, path) {
  const color = record(value, path);
  if (color.kind === "default")
    return { kind: "default" };
  if (color.kind === "palette") {
    return { kind: "palette", index: integer(color.index, `${path}.index`, 0, 255) };
  }
  if (color.kind === "rgb") {
    return {
      kind: "rgb",
      r: integer(color.r, `${path}.r`, 0, 255),
      g: integer(color.g, `${path}.g`, 0, 255),
      b: integer(color.b, `${path}.b`, 0, 255)
    };
  }
  throw new SchemaValidationError(`${path}.kind`, '"default", "palette", or "rgb"');
}
function parseStyle(value, path) {
  const style = record(value, path);
  return {
    bold: boolean(style.bold, `${path}.bold`),
    faint: boolean(style.faint, `${path}.faint`),
    italic: boolean(style.italic, `${path}.italic`),
    underline: boolean(style.underline, `${path}.underline`),
    blink: boolean(style.blink, `${path}.blink`),
    inverse: boolean(style.inverse, `${path}.inverse`),
    invisible: boolean(style.invisible, `${path}.invisible`),
    strikethrough: boolean(style.strikethrough, `${path}.strikethrough`),
    overline: boolean(style.overline, `${path}.overline`),
    fg: parseColor(style.fg, `${path}.fg`),
    bg: parseColor(style.bg, `${path}.bg`)
  };
}
function parseScreenState(value) {
  const root = record(value, "$ (phux snapshot/wait --json CLI shape)");
  const schema = integer(root.schema_version, "$.schema_version", 1, 3);
  const cols = integer(root.cols, "$.cols", 0, 65535);
  const rows = integer(root.rows, "$.rows", 0, 65535);
  const lines = strings(root.lines, "$.lines");
  if (lines.length > rows) {
    throw new SchemaValidationError("$.lines", `at most $.rows (${rows}) physical or joined logical lines`);
  }
  let cursor = null;
  if (root.cursor !== null) {
    const rawCursor = record(root.cursor, "$.cursor");
    cursor = {
      x: integer(rawCursor.x, "$.cursor.x", 0, Math.max(0, cols - 1)),
      y: integer(rawCursor.y, "$.cursor.y", 0, Math.max(0, rows - 1)),
      visible: boolean(rawCursor.visible, "$.cursor.visible")
    };
  }
  const scrollback = root.scrollback === undefined ? [] : strings(root.scrollback, "$.scrollback");
  const truncation = {
    ...root.truncated === undefined ? {} : { truncated: boolean(root.truncated, "$.truncated") },
    ...root.truncated_reason === undefined ? {} : { truncated_reason: nullableString(root.truncated_reason, "$.truncated_reason") }
  };
  if (root.cells === undefined) {
    return {
      schema_version: schema,
      pane: integer(root.pane, "$.pane", 0, 4294967295),
      cols,
      rows,
      cursor,
      lines,
      scrollback,
      ...truncation
    };
  }
  if (!Array.isArray(root.cells))
    throw new SchemaValidationError("$.cells", "an array");
  let previous = -1;
  const cells = root.cells.map((item, index) => {
    const path = `$.cells[${index}]`;
    const cell = record(item, path);
    const col = integer(cell.col, `${path}.col`, 0, Math.max(0, cols - 1));
    const row = integer(cell.row, `${path}.row`, 0, Math.max(0, rows - 1));
    const position = row * cols + col;
    if (position <= previous)
      throw new SchemaValidationError(path, "strictly row-major with no duplicate cells");
    previous = position;
    const semantic = cell.semantic;
    if (semantic !== undefined && semantic !== "output" && semantic !== "input" && semantic !== "prompt") {
      throw new SchemaValidationError(`${path}.semantic`, '"output", "input", or "prompt"');
    }
    const result = { col, row, style: parseStyle(cell.style, `${path}.style`) };
    return semantic === undefined ? result : { ...result, semantic };
  });
  return {
    schema_version: schema,
    pane: integer(root.pane, "$.pane", 0, 4294967295),
    cols,
    rows,
    cursor,
    lines,
    scrollback,
    cells,
    ...truncation
  };
}
function parseCreateResult(value) {
  const root = record(value, "$ (phux new --json CLI shape)");
  const session = string(root.session, "$.session");
  if (session.length === 0)
    throw new SchemaValidationError("$.session", "non-empty");
  return {
    session,
    terminal_id: integer(root.terminal_id, "$.terminal_id", 0, 4294967295)
  };
}
function parseSpawnResult(value) {
  const root = record(value, "$ (phux spawn --json CLI shape)");
  const satellite = nullableString(root.satellite, "$.satellite");
  if (satellite !== null && satellite.trim().length === 0) {
    throw new SchemaValidationError("$.satellite", "null or non-empty");
  }
  return {
    terminal_id: integer(root.terminal_id, "$.terminal_id", 0, 4294967295),
    satellite
  };
}
function parseLaunchResult(value) {
  const root = record(value, "$ (phux launch --json CLI shape)");
  if (root.schema_version !== 1) {
    throw new SchemaValidationError("$.schema_version", "the supported value 1");
  }
  const integration = string(root.integration, "$.integration");
  const plugin = string(root.plugin, "$.plugin");
  if (integration.trim().length === 0)
    throw new SchemaValidationError("$.integration", "non-empty");
  if (plugin.trim().length === 0)
    throw new SchemaValidationError("$.plugin", "non-empty");
  const argv = strings(root.argv, "$.argv");
  if (argv.length === 0)
    throw new SchemaValidationError("$.argv", "a non-empty array of strings");
  return {
    schema_version: 1,
    terminal_id: integer(root.terminal_id, "$.terminal_id", 0, 4294967295),
    integration,
    plugin,
    argv
  };
}
function spatialRoot(value, operation) {
  const root = record(value, `$ (phux ${operation} --json CLI shape)`);
  if (root.schema_version !== 1) {
    throw new SchemaValidationError("$.schema_version", "the supported value 1");
  }
  if (root.operation !== operation) {
    throw new SchemaValidationError("$.operation", JSON.stringify(operation));
  }
  return root;
}
function spatialDirection(value) {
  return oneOf(value, "$.direction", ["horizontal", "vertical"]);
}
function spatialRatio(value) {
  if (typeof value !== "number" || !Number.isFinite(value) || value <= 0 || value >= 1) {
    throw new SchemaValidationError("$.ratio", "finite and strictly between 0 and 1");
  }
  return value;
}
function parseInsertPaneResult(value) {
  const root = spatialRoot(value, "insert-pane");
  return {
    schema_version: 1,
    operation: "insert-pane",
    session_id: integer(root.session_id, "$.session_id", 0),
    target_terminal_id: integer(root.target_terminal_id, "$.target_terminal_id", 0, 4294967295),
    new_terminal_id: integer(root.new_terminal_id, "$.new_terminal_id", 0, 4294967295),
    direction: spatialDirection(root.direction),
    ratio: spatialRatio(root.ratio)
  };
}
function parseMovePaneResult(value) {
  const root = spatialRoot(value, "move-pane");
  return {
    schema_version: 1,
    operation: "move-pane",
    session_id: integer(root.session_id, "$.session_id", 0),
    source_terminal_id: integer(root.source_terminal_id, "$.source_terminal_id", 0, 4294967295),
    target_terminal_id: integer(root.target_terminal_id, "$.target_terminal_id", 0, 4294967295),
    direction: spatialDirection(root.direction),
    ratio: spatialRatio(root.ratio)
  };
}
function parseSwapPaneResult(value) {
  const root = spatialRoot(value, "swap-pane");
  return {
    schema_version: 1,
    operation: "swap-pane",
    session_id: integer(root.session_id, "$.session_id", 0),
    first_terminal_id: integer(root.first_terminal_id, "$.first_terminal_id", 0, 4294967295),
    second_terminal_id: integer(root.second_terminal_id, "$.second_terminal_id", 0, 4294967295)
  };
}
function parseAskedEvent(value) {
  const root = record(value, "$ (phux ask --json CLI shape)");
  if (root.event !== "asked")
    throw new SchemaValidationError("$.event", '"asked"');
  const terminal = string(root.terminal, "$.terminal");
  if (!PANE_SELECTOR.test(terminal))
    throw new SchemaValidationError("$.terminal", "a canonical pane selector");
  const elapsed = root.elapsed_seconds === null ? null : integer(root.elapsed_seconds, "$.elapsed_seconds", 0);
  return {
    event: "asked",
    terminal,
    id: string(root.id, "$.id"),
    question: string(root.question, "$.question"),
    suggestions: strings(root.suggestions, "$.suggestions"),
    elapsed_seconds: elapsed
  };
}
function parseWatchEvent(value, path = "$ (phux watch --json line)") {
  const root = record(value, path);
  const event = oneOf(root.event, `${path}.event`, [
    "title_changed",
    "command_started",
    "command_finished",
    "bell",
    "pane_spawned",
    "pane_closed",
    "dirty",
    "idle",
    "asked",
    "unknown"
  ]);
  const terminal = root.terminal === undefined ? undefined : string(root.terminal, `${path}.terminal`);
  if (terminal !== undefined && !PANE_SELECTOR.test(terminal)) {
    throw new SchemaValidationError(`${path}.terminal`, "a canonical pane selector");
  }
  const base = terminal === undefined ? {} : { terminal };
  switch (event) {
    case "title_changed":
      return { event, ...base, title: string(root.title, `${path}.title`) };
    case "command_finished":
      return {
        event,
        ...base,
        exit_code: root.exit_code === null ? null : integer(root.exit_code, `${path}.exit_code`, -2147483648, 2147483647)
      };
    case "pane_closed":
      return {
        event,
        ...base,
        exit_status: root.exit_status === null ? null : integer(root.exit_status, `${path}.exit_status`, -2147483648, 2147483647)
      };
    case "asked":
      return {
        event,
        ...base,
        id: string(root.id, `${path}.id`),
        question: string(root.question, `${path}.question`),
        suggestions: strings(root.suggestions, `${path}.suggestions`),
        elapsed_seconds: root.elapsed_seconds === null ? null : integer(root.elapsed_seconds, `${path}.elapsed_seconds`, 0)
      };
    case "unknown":
      return { event, ...base, tag: integer(root.tag, `${path}.tag`, 0) };
    default:
      return { event, ...base };
  }
}
function parseRenderedFrame(value) {
  const root = record(value, "$ (phux snapshot --rendered --json CLI shape)");
  if (root.schema_version !== 1)
    throw new SchemaValidationError("$.schema_version", "the supported value 1");
  const cols = integer(root.cols, "$.cols", 1, 65535);
  const rows = integer(root.rows, "$.rows", 1, 65535);
  if (!Array.isArray(root.cells))
    throw new SchemaValidationError("$.cells", "an array");
  const expected = cols * rows;
  if (root.cells.length !== expected)
    throw new SchemaValidationError("$.cells", `an array with exactly ${expected} entries`);
  const cells = root.cells.map((value, index) => {
    const path = `$.cells[${index}]`;
    const cell = record(value, path);
    return { grapheme: string(cell.grapheme, `${path}.grapheme`), style: parseStyle(cell.style, `${path}.style`) };
  });
  let cursor = null;
  if (root.cursor !== null) {
    const raw = record(root.cursor, "$.cursor");
    cursor = {
      x: integer(raw.x, "$.cursor.x", 0, cols - 1),
      y: integer(raw.y, "$.cursor.y", 0, rows - 1),
      visible: boolean(raw.visible, "$.cursor.visible")
    };
  }
  return { schema_version: 1, cols, rows, cursor, cells };
}
function parseRunResult(value) {
  const root = record(value, "$ (phux run --json CLI shape)");
  return {
    command: string(root.command, "$.command"),
    exit_code: integer(root.exit_code, "$.exit_code", -2147483648, 2147483647),
    output: string(root.output, "$.output"),
    duration_ms: integer(root.duration_ms, "$.duration_ms", 0),
    truncated: boolean(root.truncated, "$.truncated")
  };
}
var AGENT_KINDS = ["codex", "claude", "open_code", "pi", "omp", "plugin", "declared", "unknown"];
var AGENT_STATES = ["unknown", "idle", "working", "blocked", "done"];
var AGENT_ATTENTION = ["none", "low", "normal", "high"];
var PANE_SELECTOR = /^(?:[^/\s]+\/)?@\d+$/;
function parseAgentRecord(value, path = "$ (phux.agent/v1 record)") {
  const root = record(value, path);
  const name = string(root.name, `${path}.name`);
  if (name.trim().length === 0)
    throw new SchemaValidationError(`${path}.name`, "non-empty");
  const kind = string(root.kind, `${path}.kind`);
  if (kind.trim().length === 0)
    throw new SchemaValidationError(`${path}.kind`, "non-empty");
  const session = string(root.session, `${path}.session`);
  if (session.trim().length === 0)
    throw new SchemaValidationError(`${path}.session`, "non-empty");
  return {
    name,
    kind,
    ...root.state === undefined ? {} : { state: oneOf(root.state, `${path}.state`, AGENT_STATES) },
    ...root.attention === undefined ? {} : { attention: oneOf(root.attention, `${path}.attention`, AGENT_ATTENTION) },
    session
  };
}
function parseAgentStateList(value) {
  const root = record(value, "$ (phux agent list --json CLI shape)");
  if (root.schema_version !== 1) {
    throw new SchemaValidationError("$.schema_version", "the supported value 1");
  }
  if (!Array.isArray(root.agents))
    throw new SchemaValidationError("$.agents", "an array");
  const agents = root.agents.map((item, index) => {
    const path = `$.agents[${index}]`;
    const row = record(item, path);
    const terminal = string(row.terminal, `${path}.terminal`);
    if (!PANE_SELECTOR.test(terminal)) {
      throw new SchemaValidationError(`${path}.terminal`, "a canonical pane selector such as @3 or host/@3");
    }
    const identity = record(row.agent, `${path}.agent`);
    if (!Array.isArray(row.sources))
      throw new SchemaValidationError(`${path}.sources`, "an array");
    return {
      terminal,
      session: string(row.session, `${path}.session`),
      window: string(row.window, `${path}.window`),
      agent: {
        id: string(identity.id, `${path}.agent.id`),
        label: string(identity.label, `${path}.agent.label`),
        kind: oneOf(identity.kind, `${path}.agent.kind`, AGENT_KINDS)
      },
      ...row.agent_session === undefined ? {} : {
        agent_session: parseAgentSessionIdentity(row.agent_session, `${path}.agent_session`)
      },
      state: oneOf(row.state, `${path}.state`, AGENT_STATES),
      confidence: numberInRange(row.confidence, `${path}.confidence`, 0, 1),
      attention: oneOf(row.attention, `${path}.attention`, AGENT_ATTENTION),
      title: nullableString(row.title, `${path}.title`),
      cwd: nullableString(row.cwd, `${path}.cwd`),
      sources: row.sources.map((source, sourceIndex) => {
        const sourcePath = `${path}.sources[${sourceIndex}]`;
        const raw = record(source, sourcePath);
        return {
          kind: string(raw.kind, `${sourcePath}.kind`),
          signal: string(raw.signal, `${sourcePath}.signal`),
          confidence: numberInRange(raw.confidence, `${sourcePath}.confidence`, 0, 1),
          observed: string(raw.observed, `${sourcePath}.observed`)
        };
      }),
      explanation: string(row.explanation, `${path}.explanation`)
    };
  });
  return { schema_version: 1, agents };
}
function parseAgentSessionIdentity(value, path) {
  if (value === null)
    return null;
  const row = record(value, path);
  const resource = string(row.resource, `${path}.resource`);
  if (!PANE_SELECTOR.test(resource)) {
    throw new SchemaValidationError(`${path}.resource`, "a canonical resource selector such as @9");
  }
  return {
    resource,
    provider: string(row.provider, `${path}.provider`),
    native_id: nullableString(row.native_id, `${path}.native_id`)
  };
}
function isAgentEventType(value) {
  return AGENT_EVENT_TYPES.includes(value);
}
function parseAgentSessionOpenResult(value) {
  const root = record(value, "$ (phux agent session open --json CLI shape)");
  if (root.schema_version !== 1) {
    throw new SchemaValidationError("$.schema_version", "the supported value 1");
  }
  const resource = string(root.resource, "$.resource");
  const parent = string(root.parent, "$.parent");
  const provider = string(root.provider, "$.provider");
  if (resource.trim().length === 0)
    throw new SchemaValidationError("$.resource", "non-empty");
  if (parent.trim().length === 0)
    throw new SchemaValidationError("$.parent", "non-empty");
  if (provider.trim().length === 0)
    throw new SchemaValidationError("$.provider", "non-empty");
  return {
    schema_version: 1,
    resource,
    parent,
    provider,
    native_id: root.native_id === null || root.native_id === undefined ? null : string(root.native_id, "$.native_id")
  };
}
function parseAgentEmitResult(value) {
  const root = record(value, "$ (phux agent emit --json CLI shape)");
  if (root.schema_version !== 1) {
    throw new SchemaValidationError("$.schema_version", "the supported value 1");
  }
  const resource = string(root.resource, "$.resource");
  if (resource.trim().length === 0)
    throw new SchemaValidationError("$.resource", "non-empty");
  const type = string(root.type, "$.type");
  if (!isAgentEventType(type)) {
    throw new SchemaValidationError("$.type", AGENT_EVENT_TYPES.map((item) => JSON.stringify(item)).join(", "));
  }
  return {
    schema_version: 1,
    resource,
    seq: integer(root.seq, "$.seq", 1),
    ts_ms: integer(root.ts_ms, "$.ts_ms", 0),
    type
  };
}
function parseVersionedDocument(value) {
  const root = record(value, "$");
  integer(root.schema_version, "$.schema_version", 1);
  return root;
}
function parseAgentPromptResult(value) {
  const root = parseVersionedDocument(value);
  string(root.terminal, "$.terminal");
  oneOf(root.delivery, "$.delivery", ["acked"]);
  string(root.operation_id, "$.operation_id");
  boolean(root.transition_observed, "$.transition_observed");
  return root;
}
function parseAgentWaitResult(value) {
  const root = parseVersionedDocument(value);
  string(root.terminal, "$.terminal");
  boolean(root.satisfied, "$.satisfied");
  string(root.state, "$.state");
  return root;
}
function parseResourceWaitResult(value) {
  const root = parseVersionedDocument(value);
  string(root.resource, "$.resource");
  oneOf(root.outcome, "$.outcome", ["exited", "gone", "timed_out"]);
  nullableString(root.cursor, "$.cursor");
  boolean(root.evidence_lost, "$.evidence_lost");
  return root;
}
function parseStatusResult(value) {
  const root = parseVersionedDocument(value);
  boolean(root.running, "$.running");
  return root;
}

// ../runtime/src/adapter.ts
var MINIMUM_PHUX_VERSION = "0.1.0";
var VERSION_PATTERN = /^phux\s+v?(\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?)$/;

class PhuxCli {
  executable;
  socket;
  cwd;
  env;
  runner;
  maxStdoutBytes;
  maxStderrBytes;
  constructor(options = {}) {
    this.executable = options.executable ?? "phux";
    this.socket = options.socket;
    this.cwd = options.cwd;
    this.env = options.env;
    this.runner = options.runner ?? nodeProcessRunner;
    this.maxStdoutBytes = options.maxStdoutBytes ?? DEFAULT_MAX_OUTPUT_BYTES;
    this.maxStderrBytes = options.maxStderrBytes ?? DEFAULT_MAX_OUTPUT_BYTES;
    requireNonNegativeInteger(this.maxStdoutBytes, "maxStdoutBytes");
    requireNonNegativeInteger(this.maxStderrBytes, "maxStderrBytes");
  }
  async probe(options = {}) {
    try {
      const result = await this.execute(["--version"], options);
      if (result.termination !== "completed")
        this.throwTermination(result, [this.executable, "--version"]);
      if (result.exitCode !== 0) {
        return { available: false, reason: failureMessage(result) };
      }
      const rawVersion = result.stdout.trim();
      const match = VERSION_PATTERN.exec(rawVersion);
      if (match === null || match[1] === undefined) {
        return {
          available: false,
          rawVersion,
          reason: `unexpected version output; expected "phux X.Y.Z", got ${JSON.stringify(rawVersion)}`
        };
      }
      const version = match[1];
      if (!isCompatiblePhuxVersion(version)) {
        return {
          available: false,
          version,
          rawVersion,
          reason: `@phux/pi requires phux >= ${MINIMUM_PHUX_VERSION}; found ${version}`
        };
      }
      return { available: true, version, rawVersion };
    } catch (error) {
      if (error instanceof PhuxError && (error.code === "aborted" || error.code === "timeout" || error.code === "output_limit")) {
        throw error;
      }
      return {
        available: false,
        reason: isMissingExecutable(error) ? `phux executable ${JSON.stringify(this.executable)} was not found; install phux or configure its absolute path` : error instanceof Error ? error.message : String(error)
      };
    }
  }
  async ls(options = {}) {
    const args = this.withSocket(["ls", "--json"]);
    return this.jsonCommand("ls", args, options, parseSessionList);
  }
  async agentList(options = {}) {
    const args = this.withSocket(["agent", "list", "--json"]);
    return this.jsonCommand("agent list", args, options, parseAgentStateList);
  }
  async create(name, options = {}) {
    if (name.trim().length === 0)
      throw new TypeError("name must be non-empty");
    const args = ["new", "--json", "-s", name];
    if (options.cwd !== undefined)
      args.push("--cwd", options.cwd);
    this.pushSocket(args);
    if (options.command !== undefined) {
      if (options.command.length === 0)
        throw new TypeError("command must contain at least one argv item");
      args.push("--", ...options.command);
    }
    return this.jsonCommand("new", args, options, parseCreateResult);
  }
  async spawn(options = {}) {
    validatePlacement(options, true);
    const args = ["spawn", "--json"];
    if (options.satellite !== undefined)
      args.push("--satellite", options.satellite);
    if (options.target !== undefined)
      args.push("--target", options.target);
    if (options.split !== undefined)
      args.push("--split", options.split);
    if (options.ratio !== undefined)
      args.push("--ratio", String(options.ratio));
    if (options.cwd !== undefined)
      args.push("--cwd", options.cwd);
    if (options.retainSeconds !== undefined) {
      requirePositiveInteger(options.retainSeconds, "retainSeconds");
      args.push(`--retain=${String(options.retainSeconds)}`);
    }
    this.pushSocket(args);
    if (options.command !== undefined) {
      if (options.command.length === 0)
        throw new TypeError("command must contain at least one argv item");
      args.push("--", ...options.command);
    }
    return this.jsonCommand("spawn", args, options, parseSpawnResult);
  }
  async launch(integration, options = {}) {
    if (integration.trim().length === 0)
      throw new TypeError("integration must be non-empty");
    validatePlacement(options, false);
    const args = ["launch", "--json"];
    if (options.target !== undefined)
      args.push("--target", options.target);
    if (options.split !== undefined)
      args.push("--split", options.split);
    if (options.ratio !== undefined)
      args.push("--ratio", String(options.ratio));
    if (options.cwd !== undefined)
      args.push("--cwd", options.cwd);
    this.pushSocket(args);
    args.push(integration);
    if (options.extra !== undefined) {
      if (options.extra.length === 0)
        throw new TypeError("extra must contain at least one argv item");
      args.push("--", ...options.extra);
    }
    return this.jsonCommand("launch", args, options, parseLaunchResult);
  }
  async insertPane(target, newPane, options = {}) {
    validateSpatial(target, newPane, options);
    const args = ["insert-pane", "--json"];
    pushSpatialGeometry(args, options);
    this.pushSocket(args);
    args.push(target, newPane);
    return this.jsonCommand("insert-pane", args, options, parseInsertPaneResult);
  }
  async movePane(source, target, options = {}) {
    validateSpatial(source, target, options);
    const args = ["move-pane", "--json"];
    pushSpatialGeometry(args, options);
    this.pushSocket(args);
    args.push(source, target);
    return this.jsonCommand("move-pane", args, options, parseMovePaneResult);
  }
  async swapPane(first, second, options = {}) {
    validateDistinctTargets(first, second);
    const args = ["swap-pane", "--json"];
    this.pushSocket(args);
    args.push(first, second);
    return this.jsonCommand("swap-pane", args, options, parseSwapPaneResult);
  }
  async agentShow(options) {
    const args = ["agent", "show", "--json"];
    this.pushSocket(args);
    args.push(options.target);
    return this.jsonCommand("agent show", args, options, parseAgentStateList);
  }
  async agentSet(target, record, options = {}) {
    const args = [
      "agent",
      "set",
      target,
      "--name",
      record.name,
      "--kind",
      record.kind,
      ...record.state === undefined ? [] : ["--state", record.state],
      ...record.attention === undefined ? [] : ["--attention", record.attention],
      "--session",
      record.session
    ];
    this.pushSocket(args);
    const result = await this.completed("agent set", args, options, false);
    return parseAgentConfirmation("agent set", this.executable, result.stdout, args);
  }
  async agentClear(target, options = {}) {
    const args = ["agent", "clear", target];
    this.pushSocket(args);
    const result = await this.completed("agent clear", args, options, false);
    if (!/^@\d+\t-$/.test(result.stdout.trim())) {
      throw invalidResponse("agent clear", this.executable, args, "expected @N\\t- confirmation");
    }
  }
  async agentSessionOpen(target, options) {
    if (target.trim().length === 0)
      throw new TypeError("target must be non-empty");
    if (options.provider.trim().length === 0)
      throw new TypeError("provider must be non-empty");
    const args = ["agent", "session", "open", target, "--provider", options.provider];
    if (options.nativeId !== undefined) {
      if (options.nativeId.trim().length === 0)
        throw new TypeError("nativeId must be non-empty");
      args.push("--native-id", options.nativeId);
    }
    args.push("--json");
    this.pushSocket(args);
    return this.jsonCommand("agent session open", args, options, parseAgentSessionOpenResult);
  }
  async agentEmit(target, type, options = {}) {
    if (target.trim().length === 0)
      throw new TypeError("target must be non-empty");
    if (!isAgentEventType(type)) {
      throw new TypeError(`type must be one of ${AGENT_EVENT_TYPES.join(", ")}`);
    }
    const args = ["agent", "emit", target, "--type", type];
    if (options.data !== undefined) {
      if (typeof options.data !== "object" || Array.isArray(options.data)) {
        throw new TypeError("data must be a JSON object");
      }
      args.push("--data", JSON.stringify(options.data));
    }
    args.push("--json");
    this.pushSocket(args);
    return this.jsonCommand("agent emit", args, options, parseAgentEmitResult);
  }
  async agentSessionClose(target, options = {}) {
    if (target.trim().length === 0)
      throw new TypeError("target must be non-empty");
    const args = ["agent", "session", "close", target];
    this.pushSocket(args);
    const result = await this.completed("agent session close", args, options, false);
    return parseSessionClosed("agent session close", this.executable, result.stdout, args);
  }
  async renderedSnapshot(options) {
    requirePositiveInteger(options.cols, "cols");
    requirePositiveInteger(options.rows, "rows");
    const args = ["snapshot", "--rendered", "--json", "--cols", String(options.cols), "--rows", String(options.rows)];
    this.pushSocket(args);
    if (options.session !== undefined)
      args.push(options.session);
    return this.jsonCommand("snapshot --rendered", args, options, parseRenderedFrame);
  }
  async snapshot(options = {}) {
    const args = ["snapshot", "--json"];
    if (options.scrollback === true)
      args.push("--scrollback");
    else if (typeof options.scrollback === "number") {
      requireNonNegativeInteger(options.scrollback, "scrollback");
      args.push("--scrollback", String(options.scrollback));
    }
    if (options.cells === true)
      args.push("--cells");
    if (options.tail !== undefined) {
      requirePositiveInteger(options.tail, "tail");
      args.push("--tail", String(options.tail));
    }
    if (options.unwrap === true)
      args.push("--unwrap");
    this.pushSocket(args);
    if (options.target !== undefined)
      args.push(options.target);
    return this.jsonCommand("snapshot", args, options, parseScreenState);
  }
  async wait(options = {}) {
    const args = ["wait", "--json"];
    if ([options.until, options.regex, options.idleMs].filter((value) => value !== undefined).length > 1) {
      throw new TypeError("wait accepts only one of until, regex, or idleMs");
    }
    if (options.regex !== undefined)
      args.push("--regex", options.regex);
    if (options.outputOnly === true)
      args.push("--output-only");
    if (options.tail !== undefined) {
      requirePositiveInteger(options.tail, "tail");
      args.push("--tail", String(options.tail));
    }
    if (options.until !== undefined)
      args.push("--until", options.until);
    if (options.idleMs !== undefined) {
      requireNonNegativeInteger(options.idleMs, "idleMs");
      args.push("--idle", String(options.idleMs));
    }
    args.push("--timeout", String(operationSeconds(options)));
    this.pushSocket(args);
    if (options.target !== undefined)
      args.push(options.target);
    const result = await this.completed("wait", args, boundedExecution(options), true);
    if (result.exitCode !== 0 && result.exitCode !== 124) {
      throw commandFailed("wait", this.executable, args, result);
    }
    const screen = parseJson("wait", this.executable, result.stdout, args, parseScreenState);
    return {
      outcome: result.exitCode === 124 ? "timed_out" : "satisfied",
      screen,
      ...result.stderr.trim() === "" ? {} : { warning: result.stderr.trim().slice(0, 2048) }
    };
  }
  async run(target, command, options = {}) {
    if (command.length === 0)
      throw new TypeError("command must contain at least one argv item");
    const args = ["run", "--json"];
    args.push("--timeout", String(operationSeconds(options)));
    this.pushSocket(args);
    args.push(target, ...command);
    const result = await this.completed("run", args, boundedExecution(options), true);
    if (result.exitCode !== 0 && result.stdout.trim().length === 0) {
      throw commandFailed("run", this.executable, args, result);
    }
    const parsed = parseJson("run", this.executable, result.stdout, args, parseRunResult);
    const expectedExit = parsed.exit_code >= 0 && parsed.exit_code <= 255 ? parsed.exit_code : 255;
    if (result.exitCode !== expectedExit) {
      throw invalidResponse("run", this.executable, args, `$.exit_code (${parsed.exit_code}) does not match process exit ${String(result.exitCode)}`);
    }
    return parsed;
  }
  async sendKeys(target, keys, options = {}) {
    if (keys.length === 0)
      throw new TypeError("keys must contain at least one item");
    const args = ["send-keys"];
    this.pushSocket(args);
    args.push(target, ...keys);
    await this.completed("send-keys", args, options, false);
  }
  async paste(target, text, options = {}) {
    const args = this.withSocket(["paste"]);
    args.push("--", target, text);
    await this.completed("paste", args, options, false);
  }
  async runtimeInfo(options = {}) {
    return this.jsonCommand("runtime-info", ["runtime-info", "--json"], options, parseVersionedDocument);
  }
  async status(options = {}) {
    return this.outcomeCommand("status", this.withSocket(["status", "--json"]), options, parseStatusResult, (document) => document.running === true ? 0 : 1);
  }
  async agentPrompt(target, text, options = {}) {
    if (text.trim() === "" || /[\r\n]/.test(text))
      throw new TypeError("agent prompt requires non-empty single-line text");
    if (options.wait === false && (options.until !== undefined || options.phuxTimeoutSeconds !== undefined)) {
      throw new TypeError("until and phuxTimeoutSeconds require wait");
    }
    const args = ["agent", "prompt", "--json"];
    if (options.expectAgent !== undefined)
      args.push("--expect-agent", options.expectAgent);
    if (options.expectKind !== undefined)
      args.push("--expect-kind", options.expectKind);
    if (options.wait !== false) {
      args.push("--wait", "--timeout", String(operationSeconds(options)));
      pushUntil(args, options.until);
    }
    this.pushSocket(args);
    args.push("--", target, text);
    try {
      return await this.outcomeCommand("agent prompt", args, boundedExecution(options), parseAgentPromptResult, (document) => options.wait !== false && document.transition_observed === false ? 124 : 0);
    } catch (error) {
      if (error instanceof PhuxError && ["timeout", "aborted", "output_limit", "malformed_json", "invalid_response"].includes(error.code)) {
        throw new PhuxError(error.code, `${error.message}. Prompt delivery may have occurred. Do not resend; inspect the pane.`, {
          ...error.argv === undefined ? {} : { argv: error.argv },
          ...error.exitCode === undefined ? {} : { exitCode: error.exitCode },
          ...error.stderr === undefined ? {} : { stderr: error.stderr },
          cause: error
        });
      }
      throw error;
    }
  }
  async agentWait(target, options = {}) {
    const args = ["agent", "wait", "--json", "--timeout", String(operationSeconds(options))];
    pushUntil(args, options.until);
    this.pushSocket(args);
    args.push("--", target);
    return this.outcomeCommand("agent wait", args, boundedExecution(options), parseAgentWaitResult, (document) => document.satisfied === true ? 0 : 124);
  }
  async resourceWait(target, options = {}) {
    const args = ["resource", "wait", "--json", "--timeout", String(operationSeconds(options))];
    if (options.after !== undefined)
      args.push("--after", options.after);
    this.pushSocket(args);
    args.push("--", target);
    return this.outcomeCommand("resource wait", args, boundedExecution(options), parseResourceWaitResult, (document) => document.outcome === "exited" ? 0 : document.outcome === "gone" ? 1 : 124);
  }
  async outcomeCommand(verb, args, options, parser, expectedExit) {
    const result = await this.completed(verb, args, options, true);
    if (result.stdout.trim() === "")
      throw commandFailed(verb, this.executable, args, result);
    const document = parseJson(verb, this.executable, result.stdout, args, parser);
    if (result.exitCode !== expectedExit(document)) {
      throw invalidResponse(verb, this.executable, args, "result outcome does not match process exit status");
    }
    const warning = result.stderr.trim().slice(0, 2048);
    return warning === "" ? document : { ...document, warning };
  }
  async kill(target, options = {}) {
    const args = ["kill", "--yes", target];
    this.pushSocket(args);
    await this.completed("kill", args, options, false);
  }
  async signal(target, signal, options = {}) {
    const args = ["signal", ...signal === "terminate" || signal === "kill" ? ["--yes"] : [], target, signal];
    this.pushSocket(args);
    await this.completed("signal", args, options, false);
  }
  async tag(action, target, tags = [], options = {}) {
    if (action !== "ls" && tags.length === 0)
      throw new TypeError("tags must contain at least one item");
    if (action === "ls" && tags.length !== 0)
      throw new TypeError("tag ls does not accept tags");
    const args = ["tag", action, target, ...tags];
    this.pushSocket(args);
    const result = await this.completed(`tag ${action}`, args, options, false);
    return parseTagRows(result.stdout, `tag ${action}`, this.executable, args);
  }
  async ask(target, question, options = {}) {
    if (question.trim().length === 0)
      throw new TypeError("question must be non-empty");
    const args = ["ask", target, "--json"];
    if (options.id !== undefined)
      args.push("--id", options.id);
    for (const suggestion of options.suggestions ?? [])
      args.push("--suggest", suggestion);
    if (options.elapsedSeconds !== undefined) {
      requireNonNegativeInteger(options.elapsedSeconds, "elapsedSeconds");
      args.push("--elapsed-seconds", String(options.elapsedSeconds));
    }
    this.pushSocket(args);
    args.push(question);
    return this.jsonCommand("ask", args, options, parseAskedEvent);
  }
  async watch(options) {
    requirePositiveInteger(options.durationMs, "durationMs");
    requirePositiveInteger(options.maxEvents, "maxEvents");
    const args = ["watch", "--json"];
    this.pushSocket(args);
    args.push(options.target);
    let result;
    try {
      result = await this.execute(args, { ...options, timeoutMs: options.durationMs });
    } catch (cause) {
      throw new PhuxError("unavailable", `could not start phux executable ${JSON.stringify(this.executable)}: ${errorText(cause)}`, {
        argv: [this.executable, ...args],
        cause
      });
    }
    if (result.termination === "aborted")
      this.throwTermination(result, [this.executable, ...args]);
    if (result.termination === "output_limit")
      this.throwTermination(result, [this.executable, ...args]);
    if (result.termination === "completed" && result.exitCode !== 0) {
      throw commandFailed("watch", this.executable, args, result);
    }
    const events = parseWatchLines(result.stdout, this.executable, args);
    const truncated = events.length > options.maxEvents;
    return {
      events: truncated ? events.slice(-options.maxEvents) : events,
      truncated,
      ended: result.termination === "completed"
    };
  }
  async jsonCommand(verb, args, options, parser) {
    const result = await this.completed(verb, args, options, false);
    return parseJson(verb, this.executable, result.stdout, args, parser);
  }
  async completed(verb, args, options, allowNonzero) {
    let result;
    try {
      result = await this.execute(args, options);
    } catch (cause) {
      const message = isMissingExecutable(cause) ? `phux executable ${JSON.stringify(this.executable)} was not found; install phux or configure its absolute path` : `could not start phux executable ${JSON.stringify(this.executable)}: ${errorText(cause)}`;
      throw new PhuxError("unavailable", message, {
        argv: [this.executable, ...args],
        cause
      });
    }
    this.throwTermination(result, [this.executable, ...args]);
    if (!allowNonzero && result.exitCode !== 0) {
      throw commandFailed(verb, this.executable, args, result);
    }
    return result;
  }
  execute(args, options) {
    const request = {
      executable: this.executable,
      args,
      ...this.cwd === undefined ? {} : { cwd: this.cwd },
      ...this.env === undefined ? {} : { env: this.env },
      ...options.signal === undefined ? {} : { signal: options.signal },
      timeoutMs: options.timeoutMs ?? 1e4,
      maxStdoutBytes: this.maxStdoutBytes,
      maxStderrBytes: this.maxStderrBytes
    };
    return this.runner(request);
  }
  throwTermination(result, argv) {
    if (result.termination === "aborted") {
      throw new PhuxError("aborted", "phux command was aborted", { argv, stderr: result.stderr });
    }
    if (result.termination === "timed_out") {
      throw new PhuxError("timeout", "phux command exceeded its local subprocess timeout", {
        argv,
        stderr: result.stderr
      });
    }
    if (result.termination === "output_limit") {
      const limit = result.outputLimit === "stdout" ? this.maxStdoutBytes : this.maxStderrBytes;
      throw new PhuxError("output_limit", `phux command exceeded the ${String(limit)}-byte ${result.outputLimit} capture limit`, { argv, stderr: result.stderr });
    }
  }
  withSocket(args) {
    this.pushSocket(args);
    return args;
  }
  pushSocket(args) {
    if (this.socket !== undefined)
      args.push("--socket", this.socket);
  }
}
function operationSeconds(options) {
  const seconds = options.phuxTimeoutSeconds ?? 30;
  requirePositiveInteger(seconds, "phuxTimeoutSeconds");
  if (seconds > 86400)
    throw new RangeError("phuxTimeoutSeconds must not exceed 86400");
  return seconds;
}
function boundedExecution(options) {
  return { ...options, timeoutMs: options.timeoutMs ?? operationSeconds(options) * 1000 + 5000 };
}
function pushUntil(args, states) {
  for (const state of states ?? []) {
    if (!["idle", "working", "blocked", "done"].includes(state))
      throw new TypeError("invalid agent lifecycle state");
    args.push("--until", state);
  }
}
function parseJson(verb, executable, stdout, args, parser) {
  let value;
  try {
    value = JSON.parse(stdout);
  } catch (cause) {
    throw new PhuxError("malformed_json", `phux ${verb} returned malformed JSON: ${errorText(cause)}`, {
      argv: [executable, ...args],
      cause
    });
  }
  try {
    return parser(value);
  } catch (cause) {
    if (cause instanceof SchemaValidationError) {
      throw invalidResponse(verb, executable, args, cause.message, cause);
    }
    throw cause;
  }
}
function parseAgentConfirmation(verb, executable, stdout, args) {
  const line = stdout.trim();
  const tab = line.indexOf("\t");
  if (tab < 2 || !/^@\d+$/.test(line.slice(0, tab))) {
    throw invalidResponse(verb, executable, args, "expected @N\\t<record-json> confirmation");
  }
  return parseJson(verb, executable, line.slice(tab + 1), args, parseAgentRecord);
}
function parseSessionClosed(verb, executable, stdout, args) {
  const line = stdout.trim();
  const tab = line.indexOf("\t");
  const resource = tab < 0 ? "" : line.slice(0, tab);
  if (resource.length === 0 || line.slice(tab + 1) !== "closed") {
    throw invalidResponse(verb, executable, args, "expected @N\\tclosed confirmation");
  }
  return { resource, closed: true };
}
function parseWatchLines(stdout, executable, args) {
  const lines = stdout.split(`
`).filter((line) => line.trim().length > 0);
  return lines.map((line, index) => {
    let value;
    try {
      value = JSON.parse(line);
    } catch (cause) {
      throw new PhuxError("malformed_json", `phux watch returned malformed JSON on line ${String(index + 1)}: ${errorText(cause)}`, {
        argv: [executable, ...args],
        cause
      });
    }
    try {
      return parseWatchEvent(value, `$[${index}] (phux watch --json line)`);
    } catch (cause) {
      if (cause instanceof SchemaValidationError) {
        throw invalidResponse("watch", executable, args, cause.message, cause);
      }
      throw cause;
    }
  });
}
function parseTagRows(stdout, verb, executable, args) {
  const lines = stdout.trim().length === 0 ? [] : stdout.trim().split(`
`);
  if (lines.length === 0)
    throw invalidResponse(verb, executable, args, "expected at least one @N\\t<tag text> confirmation");
  return lines.map((line) => {
    const tab = line.indexOf("\t");
    const terminal = tab < 0 ? "" : line.slice(0, tab);
    if (!/^@\d+$/.test(terminal)) {
      throw invalidResponse(verb, executable, args, "expected @N\\t<tag text> confirmation");
    }
    return { terminal, tagsText: line.slice(tab + 1) };
  });
}
function invalidResponse(verb, executable, args, detail, cause) {
  return new PhuxError("invalid_response", `phux ${verb} JSON does not match its documented CLI shape: ${detail}`, { argv: [executable, ...args], cause });
}
function commandFailed(verb, executable, args, result) {
  let cliError;
  try {
    const document = JSON.parse(result.stderr.trim());
    if (document !== null && typeof document === "object" && !Array.isArray(document)) {
      cliError = document;
    }
  } catch {}
  return new PhuxError("command_failed", `phux ${verb} failed with exit code ${String(result.exitCode)}${diagnosticSuffix(result.stderr)}`, {
    argv: [executable, ...args],
    exitCode: result.exitCode,
    stderr: result.stderr,
    ...cliError === undefined ? {} : { cliError }
  });
}
function diagnosticSuffix(stderr) {
  const detail = stderr.trim();
  return detail.length === 0 ? "" : `: ${detail}`;
}
function failureMessage(result) {
  return `phux --version exited ${String(result.exitCode)}${diagnosticSuffix(result.stderr)}`;
}
function isCompatiblePhuxVersion(version) {
  const core = version.split(/[+-]/, 1)[0]?.split(".").map(Number);
  if (core === undefined || core.length !== 3)
    return false;
  const [major = 0, minor = 0, patch = 0] = core;
  if (major !== 0)
    return major > 0;
  if (minor !== 1)
    return minor > 1;
  if (patch !== 0)
    return patch > 0;
  return !version.includes("-");
}
function isMissingExecutable(error) {
  return error instanceof Error && "code" in error && error.code === "ENOENT";
}
function errorText(error) {
  return error instanceof Error ? error.message : String(error);
}
var SATELLITE_PANE_SELECTOR = /^[^/\s]+\/@\d+$/;
function validatePlacement(options, allowSatellite) {
  if (options.target === undefined && (options.split !== undefined || options.ratio !== undefined)) {
    throw new TypeError("target is required when split or ratio is provided");
  }
  if (options.target !== undefined) {
    if (options.target.trim().length === 0)
      throw new TypeError("target must be non-empty");
    if (SATELLITE_PANE_SELECTOR.test(options.target)) {
      throw new TypeError("explicit placement is local-only; satellite pane targets are unsupported");
    }
  }
  if (options.satellite !== undefined) {
    if (!allowSatellite)
      throw new TypeError("satellite is not supported for launch placement");
    if (options.target !== undefined)
      throw new TypeError("satellite and target placement cannot be combined");
  }
  if (options.split !== undefined && options.split !== "horizontal" && options.split !== "vertical") {
    throw new TypeError("split must be horizontal or vertical");
  }
  if (options.ratio !== undefined)
    requireRatio(options.ratio);
}
function validateSpatial(first, second, options) {
  validateDistinctTargets(first, second);
  if (options.direction !== undefined && options.direction !== "horizontal" && options.direction !== "vertical") {
    throw new TypeError("direction must be horizontal or vertical");
  }
  if (options.ratio !== undefined)
    requireRatio(options.ratio);
}
function validateDistinctTargets(first, second) {
  if (first.trim().length === 0 || second.trim().length === 0) {
    throw new TypeError("spatial targets must be non-empty");
  }
  if (first === second)
    throw new TypeError("spatial actions require two distinct targets");
  if (SATELLITE_PANE_SELECTOR.test(first) || SATELLITE_PANE_SELECTOR.test(second)) {
    throw new TypeError("spatial actions require local pane targets");
  }
}
function pushSpatialGeometry(args, options) {
  if (options.direction !== undefined)
    args.push("--split", options.direction);
  if (options.ratio !== undefined)
    args.push("--ratio", String(options.ratio));
}
function requireRatio(value) {
  if (!Number.isFinite(value) || value <= 0 || value >= 1) {
    throw new RangeError("ratio must be finite and strictly between 0 and 1");
  }
}
function requireNonNegativeInteger(value, name) {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new RangeError(`${name} must be a non-negative safe integer`);
  }
}
function requirePositiveInteger(value, name) {
  if (!Number.isSafeInteger(value) || value < 1) {
    throw new RangeError(`${name} must be a positive safe integer`);
  }
}
function hasAgentSessionCli(cli) {
  const candidate = cli;
  return typeof candidate.agentSessionOpen === "function" && typeof candidate.agentEmit === "function" && typeof candidate.agentSessionClose === "function";
}
function isAgentSessionUnsupported(error) {
  if (!(error instanceof PhuxError) || error.code !== "command_failed")
    return false;
  const haystack = `${error.message}
${error.stderr ?? ""}`;
  return /unsupported_server|unrecognized subcommand|invalid subcommand|unknown command|not a valid command/i.test(haystack);
}

class AgentSessionEmitter {
  cli;
  provider;
  onError;
  target = null;
  nativeId = null;
  resource = null;
  unavailable = false;
  constructor(cli, options) {
    this.cli = cli !== null && hasAgentSessionCli(cli) ? cli : null;
    this.provider = options.provider;
    this.onError = options.onError ?? (() => {});
    if (this.cli === null)
      this.unavailable = true;
    if (this.provider.trim().length === 0)
      throw new TypeError("provider must be non-empty");
  }
  get isOpen() {
    return this.resource !== null;
  }
  get isUnavailable() {
    return this.unavailable;
  }
  adopt(target, nativeId, identity) {
    if (identity.parent !== target || identity.provider !== this.provider || identity.native_id !== nativeId) {
      throw new Error("AgentSession identity does not match its hosting binding");
    }
    if (!/^(?:[^/\s]+\/)?@\d+$/.test(identity.resource) || identity.resource === target) {
      throw new Error("AgentSession identity must name its exact child resource");
    }
    this.target = target;
    this.nativeId = nativeId;
    this.resource = identity.resource;
  }
  async bind(target, nativeId, options = {}) {
    if (this.unavailable)
      return;
    if (target === this.target && this.isOpen && this.nativeId === nativeId)
      return;
    if (this.isOpen && (this.target !== target || this.nativeId !== nativeId)) {
      await this.finish(options);
    }
    if (target === null)
      return;
    await this.open(target, nativeId, options);
  }
  async emit(type, data, options = {}) {
    if (this.unavailable || this.resource === null || this.cli === null)
      return;
    try {
      await this.cli.agentEmit(this.resource, type, {
        ...options,
        ...data === undefined ? {} : { data }
      });
    } catch (error) {
      this.onError(error);
    }
  }
  async finish(options = {}) {
    const resource = this.resource;
    this.resource = null;
    this.target = null;
    this.nativeId = null;
    if (this.unavailable || resource === null || this.cli === null)
      return;
    try {
      await this.cli.agentEmit(resource, "session_end", options);
    } catch (error) {
      this.onError(error);
    }
    try {
      await this.cli.agentSessionClose(resource, options);
    } catch (error) {
      this.onError(error);
    }
  }
  async open(target, nativeId, options) {
    if (this.cli === null)
      return;
    try {
      const opened = await this.cli.agentSessionOpen(target, {
        provider: this.provider,
        nativeId,
        ...options
      });
      this.adopt(target, nativeId, opened);
    } catch (error) {
      if (isAgentSessionUnsupported(error)) {
        this.unavailable = true;
        return;
      }
      this.onError(error);
      return;
    }
    await this.emit("session_start", { provider: this.provider, native_id: nativeId }, options);
  }
}

// src/active-pane.ts
class ActivePane {
  lifecycle;
  sessionID;
  state;
  constructor(lifecycle) {
    this.lifecycle = lifecycle;
  }
  show(sessionID, state = "idle") {
    if (sessionID !== this.sessionID) {
      if (this.sessionID !== undefined)
        this.lifecycle.deleteSession(this.sessionID);
      this.sessionID = sessionID;
      this.state = undefined;
    }
    if (sessionID === undefined || this.state === state)
      return;
    this.state = state;
    this.lifecycle.observeState(sessionID, state);
  }
  status(sessionID, state) {
    if (sessionID === this.sessionID)
      this.show(sessionID, state);
  }
  ask(sessionID) {
    if (sessionID === this.sessionID)
      this.lifecycle.ask(sessionID);
  }
  async dispose() {
    this.show(undefined);
    await this.lifecycle.dispose();
  }
}

// src/lifecycle.ts
class OpenCodeLifecycle {
  cli;
  timeoutMs;
  onError;
  target;
  owned = new Map;
  sessions = new Map;
  openedPanes = new Map;
  tail = Promise.resolve();
  disposed = false;
  constructor(options) {
    this.cli = options.cli ?? new PhuxCli;
    this.timeoutMs = options.timeoutMs ?? 1000;
    if (!Number.isSafeInteger(this.timeoutMs) || this.timeoutMs <= 0 || this.timeoutMs > 60000) {
      throw new RangeError("lifecycle timeoutMs must be an integer from 1 through 60000");
    }
    this.onError = options.onError ?? (() => {});
    this.target = options.target;
  }
  observeState(sessionId, state) {
    if (this.disposed)
      return this.tail;
    return this.enqueue(async () => {
      await this.publish(sessionId);
      await this.emit(sessionId, state === "working" ? "prompt" : "stop");
    });
  }
  ask(sessionId, data = { kind: "permission" }) {
    if (this.disposed)
      return this.tail;
    return this.enqueue(async () => {
      await this.publish(sessionId);
      await this.emit(sessionId, "ask", data);
    });
  }
  toolStart(sessionId, toolName, toolUseId) {
    return this.toolEvent("tool_start", sessionId, toolName, toolUseId);
  }
  toolEnd(sessionId, toolName, toolUseId) {
    return this.toolEvent("tool_end", sessionId, toolName, toolUseId);
  }
  toolEvent(type, sessionId, toolName, toolUseId) {
    if (this.disposed)
      return this.tail;
    const data = {
      tool_name: toolName,
      ...toolUseId === undefined ? {} : { tool_use_id: toolUseId }
    };
    return this.enqueue(async () => {
      await this.publish(sessionId);
      await this.emit(sessionId, type, data);
    });
  }
  deleteSession(sessionId) {
    return this.enqueue(async () => {
      await this.finishSession(sessionId);
      await this.clearSession(sessionId);
    });
  }
  async dispose() {
    if (this.disposed)
      return this.tail;
    this.disposed = true;
    const sessions = [...new Set([...this.owned.keys(), ...this.sessions.keys()])];
    this.enqueue(async () => {
      for (const sessionId of sessions) {
        try {
          await this.finishSession(sessionId);
          await this.clearSession(sessionId);
        } catch (error) {
          this.onError(error);
        }
      }
    });
    await this.tail;
  }
  settled() {
    return this.tail;
  }
  enqueue(operation) {
    this.tail = this.tail.then(operation).catch((error) => {
      this.onError(error);
    });
    return this.tail;
  }
  async publish(sessionId) {
    if (this.disposed)
      return;
    const target = this.target();
    const previous = this.owned.get(sessionId);
    if (previous !== undefined && previous.target !== target) {
      await this.clearOwned(previous);
      this.owned.delete(sessionId);
    }
    if (target === undefined) {
      await this.emitter(sessionId).bind(null, sessionId, this.execution());
      return;
    }
    if (!await this.bindSession(sessionId, target))
      return;
    if (previous !== undefined && previous.target === target)
      return;
    const binding = { target, owner: `opencode:${sessionId}` };
    this.owned.set(sessionId, binding);
    await this.cli.agentSet(target, lifecycleRecord(binding.owner), this.execution());
  }
  async clearSession(sessionId) {
    const binding = this.owned.get(sessionId);
    if (binding === undefined)
      return;
    try {
      await this.clearOwned(binding);
    } finally {
      this.owned.delete(sessionId);
    }
  }
  async clearOwned(binding) {
    const projection = await this.cli.agentShow({ target: binding.target, ...this.execution() });
    const pane = projection.agents.find((candidate) => candidate.sources.some((source) => {
      if (source.kind !== "agent_record")
        return false;
      const owner = parseOwner(source.observed);
      return owner?.name === "opencode" && owner.kind === "opencode" && owner.session === binding.owner;
    }));
    if (pane === undefined)
      return;
    await this.cli.agentClear(pane.terminal, this.execution());
  }
  execution() {
    return { signal: new AbortController().signal, timeoutMs: this.timeoutMs };
  }
  emitter(sessionId) {
    const existing = this.sessions.get(sessionId);
    if (existing !== undefined)
      return existing;
    const created = new AgentSessionEmitter(hasAgentSessionCli(this.cli) ? this.cli : null, { provider: "opencode", onError: this.onError });
    this.sessions.set(sessionId, created);
    return created;
  }
  async bindSession(sessionId, target) {
    const owner = this.openedPanes.get(target);
    if (owner !== undefined && owner !== sessionId)
      return false;
    const previous = [...this.openedPanes.entries()].find((entry) => entry[1] === sessionId);
    if (previous !== undefined && previous[0] !== target)
      this.openedPanes.delete(previous[0]);
    const emitter = this.emitter(sessionId);
    await emitter.bind(target, sessionId, this.execution());
    if (emitter.isOpen)
      this.openedPanes.set(target, sessionId);
    return true;
  }
  async emit(sessionId, type, data) {
    if (this.disposed)
      return;
    const target = this.target();
    if (target !== undefined) {
      const owner = this.openedPanes.get(target);
      if (owner !== undefined && owner !== sessionId)
        return;
    }
    await this.emitter(sessionId).emit(type, data, this.execution());
  }
  async finishSession(sessionId) {
    const emitter = this.sessions.get(sessionId);
    if (emitter === undefined)
      return;
    await emitter.finish(this.execution());
    this.sessions.delete(sessionId);
    for (const [pane, owner] of this.openedPanes) {
      if (owner === sessionId)
        this.openedPanes.delete(pane);
    }
  }
}
function lifecycleRecord(owner) {
  return { name: "opencode", kind: "opencode", session: owner };
}
function parseOwner(observed) {
  try {
    const value = JSON.parse(observed);
    if (value === null || typeof value !== "object" || Array.isArray(value))
      return null;
    const row = value;
    return {
      ...typeof row.name === "string" ? { name: row.name } : {},
      ...typeof row.kind === "string" ? { kind: row.kind } : {},
      ...typeof row.session === "string" ? { session: row.session } : {}
    };
  } catch {
    return null;
  }
}

// ../runtime/src/awareness.ts
var DEFAULT_CONTEXT_MAX_BYTES = 8 * 1024;
function normalizeTerminalIdentity(value) {
  const normalized = normalizeOptional(value);
  if (normalized === null)
    return null;
  return /^\d+$/.test(normalized) ? `@${normalized}` : normalized;
}
function normalizeOptional(value) {
  if (value === undefined || value.trim().length === 0)
    return null;
  return cleanString(value, 256);
}
function cleanString(value, maxLength) {
  const cleaned = value.replace(/[\u0000-\u001f\u007f]/g, " ").replace(/\s+/g, " ").trim();
  return cleaned.length <= maxLength ? cleaned : `${cleaned.slice(0, Math.max(0, maxLength - 1))}\u2026`;
}

// src/parent.ts
function parentPane(value) {
  return normalizeTerminalIdentity(value);
}

// src/tui.ts
var tui_default = Plugin.define({
  id: "phux.tui",
  setup(ctx) {
    const parent = parentPane(process.env.PHUX_TERMINAL_ID);
    if (parent === null)
      return;
    const lifecycle = new OpenCodeLifecycle({
      cli: new PhuxCli({ env: process.env }),
      target: () => parent,
      onError: (error) => console.error("phux OpenCode presence:", error)
    });
    const active = new ActivePane(lifecycle);
    const removeSlot = ctx.ui.slot({
      append: "app",
      render: () => {
        createEffect(() => {
          const route = ctx.ui.router.current();
          const sessionID = route.type === "session" ? route.sessionID : undefined;
          active.show(sessionID, sessionID !== undefined && ctx.data.session.status(sessionID) === "running" ? "working" : "idle");
        });
        return null;
      }
    });
    const stopStatus = ctx.data.on("session.status", (event) => {
      const { sessionID, status } = event.data;
      if (status.type === "busy")
        active.status(sessionID, "working");
      if (status.type === "idle")
        active.status(sessionID, "idle");
    });
    const stopIdle = ctx.data.on("session.idle", (event) => active.status(event.data.sessionID, "idle"));
    const stopPermission = ctx.data.on("permission.asked", (event) => active.ask(event.data.sessionID));
    return async () => {
      stopStatus();
      stopIdle();
      stopPermission();
      removeSlot();
      await active.dispose();
    };
  }
});
export {
  tui_default as default
};
