import { appendFileSync, mkdirSync } from "node:fs";
import { hostname, userInfo } from "node:os";
import { join } from "node:path";

import { BACKGROUND_CONTEXT } from "@earendil-works/chord/context";
import { ModelRuntime } from "@earendil-works/pi-coding-agent";
import { createRegistry, Harness, watchEvents, type Conversation } from "@earendil-works/pi-durable";
import { openNodeSqliteStorage } from "@earendil-works/pi-durable/storage/sqlite/node";
import { PhuxCli } from "@phux/integration-runtime/adapter";
import { normalizeTerminalIdentity } from "@phux/integration-runtime/awareness";
import { MessageTranscript } from "@phux/integration-runtime/message-transcript";
import * as undici from "undici";

import { Chief, CHIEF_VERSION } from "./chief.js";
import { chiefPaths, loadConfig, shortHost, splitModel } from "./config.js";
import { chiefFinding, ErrorIntake, type Finding } from "./diagnostics.js";
import { DISABLE_BRACKETED_PASTE, ENABLE_BRACKETED_PASTE, LineInput } from "./line-input.js";
import { createPersona } from "./persona.js";
import { ChiefStream } from "./phux-stream.js";
import { createChiefTools } from "./tools.js";

const context = BACKGROUND_CONTEXT;
const INTAKE_INTERVAL_MS = 5 * 60_000;

export interface RunOptions {
  readonly env?: NodeJS.ProcessEnv;
  readonly input?: NodeJS.ReadStream;
  readonly output?: NodeJS.WriteStream;
}

/** pi's HTTP setup: one undici agent with long body timeouts, so provider streams are not cut off. */
function configureHttp(): void {
  const dispatcher = new undici.EnvHttpProxyAgent({
    allowH2: false,
    bodyTimeout: 300_000,
    connect: { autoSelectFamilyAttemptTimeout: 2_000 },
  });
  // Undici can emit an internal error while a mid-stream body is torn down; the read still rejects.
  (dispatcher as unknown as NodeJS.EventEmitter).on("error", () => {});
  undici.setGlobalDispatcher(dispatcher);
}

export async function runChief(options: RunOptions = {}): Promise<void> {
  const env = options.env ?? process.env;
  const input = options.input ?? process.stdin;
  const output = options.output ?? process.stdout;
  const paths = chiefPaths(env);
  mkdirSync(paths.home, { recursive: true });
  const config = loadConfig(paths, env);
  const log = (line: string) => {
    try {
      appendFileSync(paths.log, `${new Date().toISOString()} ${line}\n`);
    } catch {
      // Logging must never stop the chief.
    }
  };
  const print = (line: string) => output.write(`${line.replace(/\r?\n/g, "\r\n")}\r\n`);

  configureHttp();
  const hostPane = normalizeTerminalIdentity(env.PHUX_TERMINAL_ID) ?? undefined;
  const cli = new PhuxCli({
    executable: env.PHUX_BIN || "phux",
    ...(env.PHUX_SOCKET ? { socket: env.PHUX_SOCKET } : {}),
    cwd: config.cwd,
    env,
  });
  const host = shortHost(hostname());
  const models = await ModelRuntime.create({ authPath: config.authPath });
  const registry = createRegistry();
  const tools = createChiefTools({ cli, hostPane, env });
  const persona = createPersona(config, paths, { owner: userInfo().username, pane: hostPane, host });
  registry.install(tools);
  registry.install(persona);

  const intake = new ErrorIntake({ statePath: paths.diagnostics, ownedRepos: config.ownedRepos });
  const pendingFindings: Finding[] = [];
  const harness = await Harness.open(await openNodeSqliteStorage(paths.database), {
    models,
    registry,
    settings: {
      compaction: { enabled: true, reserveTokens: 16_384, keepRecentTokens: 20_000 },
      retry: { maxRetries: 3 },
      followUpMode: "one-at-a-time",
    },
    onReport: (error) => {
      const message = error instanceof Error ? error.message : String(error);
      log(`report: ${message}`);
      pendingFindings.push(...intake.admit([chiefFinding(message)]));
    },
  }, context);
  const { provider, modelId } = splitModel(config.model);
  const root = await harness.root(context, {
    agent: { model: { provider, modelId }, thinkingLevel: config.thinking, cwd: config.cwd },
  });

  const stream = new ChiefStream(cli, hostPane, "chief", (error) => log(`phux: ${error instanceof Error ? error.message : String(error)}`), join(paths.home, "agent-session.json"));
  await stream.bind();
  if (hostPane !== undefined && !stream.bound) log("AgentSession did not open; the phone sees the pane but no transcript");
  const transcript = new MessageTranscript("pi");
  // A new process opens a new AgentSession; replay the recent thread into it so a client sees one continuous conversation.
  await replayHistory(root, stream);
  const chief = new Chief({ config, paths, harness, root, models, stream, transcript, print, log, host, hostPane, intake, pendingFindings });

  const events = await watchEvents(harness, root.id, context);
  // A run the previous process left unfinished resumes without a new run_start.
  if (events.snapshot.run !== undefined) {
    chief.resumedRun();
    await stream.state("working");
  }
  events.start(async (batch) => {
    for (const event of batch) await chief.onEvent(event);
  });
  harness.resume();

  const lines = new LineInput();
  const isTty = "setRawMode" in input && input.isTTY;
  if (isTty) input.setRawMode(true);
  output.write(ENABLE_BRACKETED_PASTE);
  print(`phux chief ${CHIEF_VERSION} on ${host}${hostPane ? ` ${hostPane}` : ""} · ${config.model} · yolo ${config.yolo ? "on" : "off"} · /help`);
  await chief.publishStatus();

  const done = new Promise<void>((resolve) => {
    input.setEncoding("utf8");
    input.on("data", (chunk: string) => {
      for (const event of lines.feed(chunk)) {
        if (event.type === "echo") output.write(event.text);
        else if (event.type === "interrupt") void chief.handle({ type: "abort", id: undefined });
        else void chief.submitText(event.text);
      }
    });
    input.on("end", resolve);
    for (const signal of ["SIGTERM", "SIGHUP", "SIGINT"] as const) process.once(signal, () => resolve());
  });

  const intakeTimer = setInterval(() => void chief.runIntake(), INTAKE_INTERVAL_MS);
  void chief.runIntake();
  await done;
  clearInterval(intakeTimer);
  output.write(DISABLE_BRACKETED_PASTE);
  if (isTty) input.setRawMode(false);
  await events.stop();
  // Close writes no outcome: an interrupted turn resumes on the next start.
  await harness.close(context);
  await stream.close();
  await stream.settled();
}

const REPLAY_MESSAGES = 40;

/** The last user and assistant messages of the active context, as final transcript entries. */
export async function replayHistory(root: Conversation, stream: ChiefStream): Promise<void> {
  const { messages } = await root.context(context);
  const transcript = new MessageTranscript("pi");
  const recent = messages.filter((message) => message.role === "user" || message.role === "assistant").slice(-REPLAY_MESSAGES);
  for (const message of recent) {
    for (const data of transcript.messageEnd(message)) await stream.emit("provider_raw", data);
  }
}
