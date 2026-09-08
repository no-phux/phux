import type { CircuitConfig } from "./native-admission";

interface NativeSession<ResponseLike> {
  prepare(sid: string, expiresAt: number, startupTimeoutMs: number): Promise<void>;
  fetch(request: Request): Promise<ResponseLike>;
  abortStartup(): Promise<void>;
}

interface NativeCap {
  nativeFailed(
    sid: string,
    edgeTtlMs: number,
    circuitConfig: CircuitConfig,
  ): Promise<void>;
  nativeSucceeded(sid: string): Promise<void>;
}

type NativeFailureReason =
  | "startup-timeout"
  | "startup-error"
  | `pre-upgrade-${number}`;

export type NativeStartResult<ResponseLike> =
  | { response: ResponseLike }
  | { fallbackReason: NativeFailureReason };

export interface RelaySocket {
  send(data: string | ArrayBuffer | ArrayBufferView): void;
  close(code?: number, reason?: string): void;
  addEventListener(type: "message", listener: (event: MessageEvent) => void): void;
  addEventListener(type: "close", listener: (event: CloseEvent) => void): void;
  addEventListener(type: "error", listener: (event: Event) => void): void;
}

export function relaySessionSockets(
  browser: RelaySocket,
  upstream: RelaySocket,
  firstFrame: string,
  onClosed: () => void,
): void {
  let closed = false;
  const finish = (): boolean => {
    if (closed) return false;
    closed = true;
    onClosed();
    return true;
  };
  browser.send(firstFrame);
  browser.addEventListener("message", (event) => {
    if (event.data instanceof ArrayBuffer || ArrayBuffer.isView(event.data))
      upstream.send(event.data);
    else upstream.close(1002, "invalid client frame");
  });
  upstream.addEventListener("message", (event) => {
    if (event.data instanceof ArrayBuffer || ArrayBuffer.isView(event.data))
      browser.send(event.data);
    else if (finish()) {
      browser.close(1011, "native session protocol error");
      upstream.close(1011, "upstream protocol error");
    }
  });
  browser.addEventListener("close", () => {
    if (finish()) upstream.close(1000, "client closed");
  });
  upstream.addEventListener("close", (event) => {
    if (!finish()) return;
    const code = event.code === 1000 || event.code === 1001 ? event.code : 1011;
    browser.close(code, code === 1011 ? "native session ended" : "");
  });
  browser.addEventListener("error", () => {
    if (finish()) upstream.close(1011, "client connection error");
  });
  upstream.addEventListener("error", () => {
    if (finish()) browser.close(1011, "native session ended");
  });
}

export async function startNative<ResponseLike extends { status: number }>(options: {
  sid: string;
  expiresAt: number;
  startupTimeoutMs: number;
  edgeTtlMs: number;
  circuitConfig: CircuitConfig;
  request: Request;
  session: NativeSession<ResponseLike>;
  cap: NativeCap;
}): Promise<NativeStartResult<ResponseLike>> {
  let fallbackReason: NativeFailureReason;
  try {
    await options.session.prepare(
      options.sid,
      options.expiresAt,
      options.startupTimeoutMs,
    );
    const response = await options.session.fetch(options.request);
    if (response.status === 101) {
      await options.cap.nativeSucceeded(options.sid);
      return { response };
    }
    fallbackReason = `pre-upgrade-${response.status}`;
  } catch (error) {
    fallbackReason =
      error instanceof DOMException && error.name === "TimeoutError"
        ? "startup-timeout"
        : "startup-error";
  }

  await options.cap.nativeFailed(
    options.sid,
    options.edgeTtlMs,
    options.circuitConfig,
  );
  try {
    await options.session.abortStartup();
  } catch {
    // The reservation is already edge-only, so a late stop is harmless.
  }
  return { fallbackReason };
}
