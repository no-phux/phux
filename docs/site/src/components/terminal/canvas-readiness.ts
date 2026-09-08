const DEFAULT_TIMEOUT_MS = 12_000;
const DEFAULT_POLL_INTERVAL_MS = 100;
const MIN_NON_DOMINANT_SAMPLES = 256;
type Timer = number | ReturnType<typeof setTimeout>;

interface CanvasLike {
  width: number;
  height: number;
  getContext(contextId: "2d"): {
    getImageData(x: number, y: number, width: number, height: number): ImageData;
  } | null;
}

interface ReadinessOptions {
  timeoutMs?: number;
  pollIntervalMs?: number;
  signal?: AbortSignal;
  now?: () => number;
  setTimer?: (
    callback: () => void,
    delay: number,
  ) => Timer;
  clearTimer?: (timer: Timer) => void;
}

export function hasMeaningfulCanvasPaint(canvas: CanvasLike): boolean {
  if (canvas.width === 0 || canvas.height === 0) return false;

  const context = canvas.getContext("2d");
  if (!context) throw new Error("2D canvas context unavailable");

  const pixels = context.getImageData(0, 0, canvas.width, canvas.height).data;
  const sampleCount = Math.ceil(pixels.length / 8);
  const requiredSamples = Math.min(
    MIN_NON_DOMINANT_SAMPLES,
    Math.max(1, Math.floor(sampleCount * 0.1)),
  );
  const colors = new Map<number, number>();
  let samplesSeen = 0;
  let dominantCount = 0;

  // Sample every other pixel and count colors rather than comparing with the
  // top-left pixel, which may be occupied by the blinking cursor. A real frame
  // has hundreds of non-background glyph samples; a cursor cell does not.
  for (let index = 0; index < pixels.length; index += 8) {
    const color =
      ((pixels[index] << 24) |
        (pixels[index + 1] << 16) |
        (pixels[index + 2] << 8) |
        pixels[index + 3]) >>>
      0;
    const count = (colors.get(color) ?? 0) + 1;
    colors.set(color, count);
    samplesSeen++;
    dominantCount = Math.max(dominantCount, count);
    if (samplesSeen - dominantCount >= requiredSamples) return true;
  }

  return false;
}

export function waitForMeaningfulCanvasPaint(
  canvas: CanvasLike,
  options: ReadinessOptions = {},
): Promise<void> {
  const timeoutMs = options.timeoutMs ?? DEFAULT_TIMEOUT_MS;
  const pollIntervalMs = options.pollIntervalMs ?? DEFAULT_POLL_INTERVAL_MS;
  const now = options.now ?? Date.now;
  const setTimer = options.setTimer ?? setTimeout;
  const clearTimer = options.clearTimer ?? clearTimeout;
  const deadline = now() + timeoutMs;

  return new Promise((resolve, reject) => {
    let timer: Timer | undefined;

    const finish = (error?: Error) => {
      if (timer !== undefined) clearTimer(timer);
      options.signal?.removeEventListener("abort", onAbort);
      if (error) reject(error);
      else resolve();
    };
    const onAbort = () => finish(new DOMException("Aborted", "AbortError"));
    const check = () => {
      try {
        if (hasMeaningfulCanvasPaint(canvas)) {
          finish();
          return;
        }
      } catch (error) {
        finish(error instanceof Error ? error : new Error(String(error)));
        return;
      }

      const remaining = deadline - now();
      if (remaining <= 0) {
        finish(new Error("Terminal did not paint before the readiness deadline"));
        return;
      }
      timer = setTimer(check, Math.min(pollIntervalMs, remaining));
    };

    if (options.signal?.aborted) {
      onAbort();
      return;
    }
    options.signal?.addEventListener("abort", onAbort, { once: true });
    check();
  });
}
