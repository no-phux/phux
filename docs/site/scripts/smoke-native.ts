import {
  ATTACH_FRAME,
  HELLO_FRAME,
  inputTextFrames,
  parseTerminalPayload,
} from "./phux-wire";
import {
  parseSessionInfo,
  type PublicFallbackReason,
} from "../worker/src/session-info";

const endpoint = process.env.PHUX_NATIVE_WS ??
  "wss://shell.phux.sh/session?mode=native";
const allowedOrigin = process.env.PHUX_ALLOWED_ORIGIN ?? "https://phux.sh";
const exerciseFallback = process.argv.includes("--exercise-fallback");
const expectEdge = process.argv.includes("--expect-edge");
const timeoutMs = Number(process.env.PHUX_SMOKE_TIMEOUT_MS ?? 30_000);
const nativeGreeting = "Real disposable Linux shell";
const commandMarker = "PHUX_NATIVE_OK";
const edgeGreeting = process.env.PHUX_EDGE_MARKER ?? "instant edge tour";
const expectedFallback = process.env.PHUX_EXPECT_FALLBACK as
  | PublicFallbackReason
  | undefined;
const requestedMode = new URL(endpoint).searchParams.get("mode") ?? "demo";
const syntheticToken = process.env.PHUX_SYNTHETIC_TOKEN;

type ExpectedBackend = "native" | "edge";

const BunWebSocket = WebSocket as unknown as new (
  url: string,
  options: { headers: Record<string, string> },
) => WebSocket;

async function aggregateStatus(): Promise<string> {
  try {
    const url = new URL(endpoint.replace(/^ws/, "http"));
    url.pathname = "/healthz";
    url.search = "";
    const value = (await (await fetch(url)).json()) as Record<string, unknown>;
    return `live=${value.live} native_live=${value.nativeLive} circuit=${value.circuit}`;
  } catch {
    return "status=unavailable";
  }
}

async function connect(expected: ExpectedBackend, keepOpen = false): Promise<WebSocket> {
  const startedAt = Date.now();
  const socket = new BunWebSocket(endpoint, {
    headers: {
      Origin: allowedOrigin,
      ...(syntheticToken ? { Authorization: `Bearer ${syntheticToken}` } : {}),
    },
  });
  socket.binaryType = "arraybuffer";

  await new Promise<void>((resolve, reject) => {
    let output = "";
    let terminalId: Uint8Array | undefined;
    let commandSent = false;
    let receivedSessionInfo = false;
    let settled = false;
    const finish = (error?: Error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      if (error) reject(error);
      else resolve();
    };
    const timeout = setTimeout(async () => {
      finish(
        new Error(
          `${expected} protocol smoke timed out native_marker=${output.includes(nativeGreeting)} edge_marker=${output.includes(edgeGreeting)} ${await aggregateStatus()}`,
        ),
      );
      socket.close(1000, "smoke timeout");
    }, timeoutMs);

    socket.addEventListener("open", () => {
      socket.send(HELLO_FRAME);
      socket.send(ATTACH_FRAME);
    });
    socket.addEventListener("message", (event) => {
      if (!receivedSessionInfo) {
        if (typeof event.data !== "string") {
          finish(new Error("phux server did not send session metadata first"));
          return;
        }
        const info = parseSessionInfo(event.data);
        if (!info || info.backend !== expected || info.expiresAt <= Date.now()) {
          finish(new Error("phux server returned invalid session metadata"));
          return;
        }
        if (
          expectedFallback !== undefined &&
          info.fallbackReason !== expectedFallback
        ) {
          finish(
            new Error(
              `expected fallback ${expectedFallback}, received ${info.fallbackReason ?? "none"}`,
            ),
          );
          return;
        }
        if (expected === "native" && info.fallbackReason !== undefined) {
          finish(new Error("native session metadata included a fallback reason"));
          return;
        }
        if (
          expected === "edge" &&
          requestedMode === "native" &&
          info.fallbackReason === undefined
        ) {
          finish(new Error("native fallback metadata omitted its reason"));
          return;
        }
        if (
          expected === "edge" &&
          requestedMode !== "native" &&
          info.fallbackReason !== undefined
        ) {
          finish(new Error("direct edge metadata included a fallback reason"));
          return;
        }
        receivedSessionInfo = true;
        return;
      }
      if (!(event.data instanceof ArrayBuffer)) {
        finish(new Error("phux server returned a non-binary phux frame"));
        return;
      }
      try {
        const payload = parseTerminalPayload(new Uint8Array(event.data));
        if (!payload) return;
        terminalId ??= payload.terminalId;
        output += new TextDecoder().decode(payload.bytes);
        if (
          expected === "native" &&
          output.includes(nativeGreeting) &&
          terminalId &&
          !commandSent
        ) {
          commandSent = true;
          for (const frame of inputTextFrames(terminalId, `printf ${commandMarker}`))
            socket.send(
              frame.buffer.slice(
                frame.byteOffset,
                frame.byteOffset + frame.byteLength,
              ) as ArrayBuffer,
            );
        }
        const verified =
          expected === "native"
            ? output.includes(nativeGreeting) && output.includes(commandMarker)
            : output.includes(edgeGreeting);
        if (verified) finish();
        else if (expected === "edge" && output.includes(nativeGreeting))
          finish(new Error("expected edge backend but received native marker"));
        else if (expected === "native" && output.includes(edgeGreeting))
          finish(new Error("expected native backend but received edge marker"));
      } catch (error) {
        finish(error instanceof Error ? error : new Error("wire parser failed"));
      }
    });
    socket.addEventListener("error", () =>
      finish(new Error(`${expected} websocket failed before verification`)),
    );
    socket.addEventListener("close", (event) => {
      if (!settled)
        finish(
          new Error(
            `${expected} websocket closed before verification code=${event.code}`,
          ),
        );
    });
  });

  console.log(`${expected}_smoke verified=true open_ms=${Date.now() - startedAt}`);
  if (!keepOpen) socket.close(1000, "smoke complete");
  return socket;
}

if (exerciseFallback) {
  const native = await connect("native", true);
  await connect("edge");
  native.close(1000, "fallback exercise complete");
} else if (expectEdge) {
  await connect("edge");
} else {
  await connect("native");
}

export {};
