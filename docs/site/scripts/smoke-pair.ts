// Pairing smoke: one edge session, two screens.
//
// Opens a demo session exactly as the landing page does, reads the single-use
// attach code out of the session envelope, attaches a *second* socket with it,
// and proves both views share one shell: input typed on the second screen comes
// back on the first. Also proves the code spends once and that an attachment
// reserves no session slot.
//
// Needs no secret: the edge demo is anonymous by design, which is the point of
// pairing this way. Runs against a deployed demo Worker, or PHUX_NATIVE_WS.

import {
  ATTACH_FRAME,
  HELLO_FRAME,
  TYPE_ERROR,
  frameType,
  inputTextFrames,
  parseTerminalPayload,
} from "./phux-wire";
import { TYPE_ATTACHED, TYPE_ATTACH_READY, TYPE_HELLO_OK } from "../worker/src/wire";
import { parseSessionInfo, type SessionInfo } from "../worker/src/session-info";

const endpoint =
  process.env.PHUX_NATIVE_WS ?? "wss://shell.phux.sh/session?mode=demo";
const allowedOrigin = process.env.PHUX_ALLOWED_ORIGIN ?? "https://phux.sh";
const timeoutMs = Number(process.env.PHUX_SMOKE_TIMEOUT_MS ?? 30_000);
const marker = "PHUX_PAIR_OK";
/** The edge shell's dialect: `echo` prints its argument. */
const command = `echo ${marker}`;

const sessionUrl = new URL(endpoint);
const originUrl = new URL(sessionUrl);
originUrl.protocol = sessionUrl.protocol === "wss:" ? "https:" : "http:";

const BunWebSocket = WebSocket as unknown as new (
  url: string,
  options: { headers: Record<string, string> },
) => WebSocket;

function fail(message: string): never {
  console.error(`pair_smoke failed: ${message}`);
  process.exit(1);
}

function originUrlOf(pathname: string): URL {
  const url = new URL(originUrl);
  url.pathname = pathname;
  url.search = "";
  return url;
}

/** The smoke client's pinned ATTACH, retargeted at one view's correlation id. */
function attachFrame(attachId: number): Uint8Array {
  const frame = ATTACH_FRAME.slice();
  // The frame ends with `field 5 (attach id), wire type 4, length 4, u32 1`.
  const header = frame.slice(frame.length - 7, frame.length - 4);
  if (header[0] !== 5 || header[1] !== 4 || header[2] !== 4)
    fail("the pinned ATTACH frame no longer ends with its attach id");
  frame[frame.length - 4] = (attachId >>> 24) & 255;
  frame[frame.length - 3] = (attachId >>> 16) & 255;
  frame[frame.length - 2] = (attachId >>> 8) & 255;
  frame[frame.length - 1] = attachId & 255;
  return frame;
}

async function pairStatus(code: string): Promise<Response> {
  const probe = originUrlOf("/session/pair");
  probe.searchParams.set("code", code);
  return fetch(probe, { headers: { Origin: allowedOrigin } });
}

async function liveSessions(): Promise<number> {
  const response = await fetch(originUrlOf("/healthz"));
  const value = (await response.json()) as Record<string, unknown>;
  return Number(value.live ?? -1);
}

async function waitFor(predicate: () => boolean, what: string): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  fail(`timed out waiting for ${what}`);
}

interface View {
  socket: WebSocket;
  envelope: SessionInfo;
  terminalId: Uint8Array;
  output: () => string;
  /** Count of frames of one type this view has received. */
  seen: (type: number) => number;
  close: () => void;
}

/**
 * Attach one view; resolves once its own attachment is rendering. `attachId` is
 * the client-chosen correlation id this view's ATTACH carries, which is how the
 * session knows which returned frames belong to it.
 */
function open(label: string, attach: string, attachId: number): Promise<View> {
  const target = new URL(sessionUrl);
  if (attach) target.searchParams.set("attach", attach);
  const socket = new BunWebSocket(target.toString(), {
    headers: { Origin: allowedOrigin },
  });
  socket.binaryType = "arraybuffer";

  let text = "";
  let envelope: SessionInfo | undefined;
  let terminalId: Uint8Array | undefined;
  let helloOk = false;
  let addressed = 0;
  const counts = new Map<number, number>();

  return new Promise<View>((resolve, reject) => {
    const timeout = setTimeout(
      () =>
        reject(
          new Error(
            `${label} timed out before rendering (addressed_frames=${addressed} bytes=${text.length})`,
          ),
        ),
      timeoutMs,
    );
    // A view is ready when the session has addressed it *and* it has painted
    // something. Those arrive in different frames in either order, so this is
    // checked after each one rather than inside the output branch.
    const settle = () => {
      if (!envelope || !terminalId || addressed < 2) return;
      clearTimeout(timeout);
      resolve({
        socket,
        envelope,
        terminalId,
        output: () => text,
        seen: (type: number) => counts.get(type) ?? 0,
        close: () => socket.close(1000, "smoke complete"),
      });
    };
    socket.addEventListener("message", (event) => {
      if (!envelope) {
        if (typeof event.data !== "string") {
          reject(new Error(`${label} received binary data before its envelope`));
          return;
        }
        envelope = parseSessionInfo(event.data);
        if (!envelope) {
          reject(new Error(`${label} received an invalid session envelope`));
          return;
        }
        socket.send(HELLO_FRAME);
        return;
      }
      if (!(event.data instanceof ArrayBuffer)) return;
      const frame = new Uint8Array(event.data);
      const type = frameType(frame);
      if (type !== undefined) counts.set(type, (counts.get(type) ?? 0) + 1);
      if (type === TYPE_ERROR) {
        reject(new Error(`${label} received a protocol error`));
        return;
      }
      // HELLO first, then this view's own ATTACH; the session answers in order.
      if (!helloOk) {
        if (type !== TYPE_HELLO_OK) return;
        helloOk = true;
        const attach = attachFrame(attachId);
        socket.send(
          attach.buffer.slice(
            attach.byteOffset,
            attach.byteOffset + attach.byteLength,
          ) as ArrayBuffer,
        );
        return;
      }
      // The session answers ATTACH with ATTACHED, this view's bootstrap, and
      // ATTACH_READY. Those two addressed frames must reach *this* view: if the
      // routing loses them the real client stalls on "protocol attach timed
      // out", so the smoke refuses to call a view ready without them.
      if (type === TYPE_ATTACHED || type === TYPE_ATTACH_READY) addressed += 1;
      const payload = parseTerminalPayload(frame);
      if (payload) {
        terminalId ??= payload.terminalId;
        text += new TextDecoder().decode(payload.bytes);
      }
      settle();
    });
    socket.addEventListener("error", () =>
      reject(new Error(`${label} failed to connect`)),
    );
    socket.addEventListener("close", (event) =>
      reject(new Error(`${label} closed before rendering code=${event.code}`)),
    );
  });
}

/** Attach with a spent code: the session must close the view, not serve it. */
function expectRejected(attach: string): Promise<number> {
  const target = new URL(sessionUrl);
  target.searchParams.set("attach", attach);
  const socket = new BunWebSocket(target.toString(), {
    headers: { Origin: allowedOrigin },
  });
  return new Promise<number>((resolve) => {
    const timeout = setTimeout(() => resolve(-1), timeoutMs);
    const settle = (code: number) => {
      clearTimeout(timeout);
      resolve(code);
    };
    socket.addEventListener("close", (event) => settle(event.code));
    socket.addEventListener("error", () => settle(-1));
    socket.addEventListener("message", (event) => {
      if (typeof event.data === "string" && parseSessionInfo(event.data)) settle(0);
      else settle(-2);
    });
  });
}

const desktop = await open("desktop", "", 5000);
console.log(`pair_smoke attached_session backend=${desktop.envelope.backend}`);
// Baseline with exactly one view attached: the second view must not add a slot.
const liveWithOneView = await liveSessions();
if (liveWithOneView < 1) fail("the session reserved no slot at all");

const code = desktop.envelope.pair?.code;
if (!code) fail("the session envelope carried no attach code");
if (desktop.envelope.pair!.expiresAt > desktop.envelope.expiresAt)
  fail("the attach code outlives its session");

const pending = await pairStatus(code);
if (!pending.ok) fail(`pairing status returned ${pending.status}`);
if ((await pending.json() as { linked: boolean }).linked)
  fail("a fresh pairing code reported itself already linked");

const phone = await open("phone", code, 6000);
if (phone.envelope.pair) fail("the paired view was handed its own attach code");
if (phone.envelope.backend !== desktop.envelope.backend)
  fail("the two views disagree about their backend");
console.log(`pair_smoke attached_second_view backend=${phone.envelope.backend}`);

// A handshake reply belongs to the connection that asked. Broadcasting one
// view's HELLO_OK to another is a protocol error to the real client, which then
// closes the socket and takes the session with it.
if (desktop.seen(TYPE_HELLO_OK) !== 1)
  fail(`the first view saw ${desktop.seen(TYPE_HELLO_OK)} handshake replies`);
if (phone.seen(TYPE_HELLO_OK) !== 1)
  fail(`the paired view saw ${phone.seen(TYPE_HELLO_OK)} handshake replies`);
console.log("pair_smoke handshake_frames_addressed=true");

// Typing on the phone has to come back on the desktop: one shell, two screens.
for (const frame of inputTextFrames(phone.terminalId, command)) {
  phone.socket.send(
    frame.buffer.slice(
      frame.byteOffset,
      frame.byteOffset + frame.byteLength,
    ) as ArrayBuffer,
  );
}
await waitFor(() => desktop.output().includes(marker), "the phone's input on the desktop view");
await waitFor(() => phone.output().includes(marker), "the phone's own echo");
console.log("pair_smoke verified=true shared_shell=true");

const claimed = await pairStatus(code);
if (!claimed.ok) fail(`pairing status returned ${claimed.status}`);
if (!(await claimed.json() as { linked: boolean }).linked)
  fail("a spent pairing code did not report itself linked");

const replay = await expectRejected(code);
if (replay !== 4003) fail(`a spent pairing code was served again (close ${replay})`);

const invalid = await pairStatus("not-a-real-code");
if (invalid.status !== 400) fail(`an invalid code returned ${invalid.status}`);
const foreign = await expectRejected("not-a-real-code");
if (foreign !== 4003) fail(`an invalid code was served (close ${foreign})`);

const liveAfter = await liveSessions();
if (liveAfter !== liveWithOneView)
  fail(`pairing changed the live slot count: ${liveWithOneView} -> ${liveAfter}`);
console.log("pair_smoke verified=true single_use=true attach_reserves_nothing=true");

phone.close();
desktop.close();
process.exit(0);
