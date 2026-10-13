// Browser-only UI fixture. Served by verify-terminal.ts in place of the WASM
// module; never imported by production code. This is not a wire/runtime test.
export default async function init() {}

function telemetry() {
  window.__terminalFixture ??= { starts: 0, closes: 0, frees: 0, aborted: 0, actions: [] };
  return window.__terminalFixture;
}
function waitForAbort(signal) {
  return new Promise((_, reject) => signal.addEventListener("abort", () => {
    telemetry().aborted++;
    reject(new DOMException("Aborted", "AbortError"));
  }, { once: true }));
}
export async function start_hosted(endpoint, id, cols, rows, onEvent, signal) {
  const log = telemetry();
  log.starts++;
  log.mode = new URL(endpoint).searchParams.get("mode");
  const scenario = window.__terminalScenario ?? "live";
  if (scenario === "stalled") return waitForAbort(signal);
  const canvas = document.getElementById(id);
  canvas.dataset.phuxCellWidth = "9";
  canvas.dataset.phuxCellHeight = "16";
  let count = 1;
  let focused = 1;
  let closed = false;
  function paint() {
    const ctx = canvas.getContext("2d");
    ctx.fillStyle = "#0b0b0d";
    ctx.fillRect(0, 0, canvas.width, canvas.height);
    ctx.font = "13px monospace";
    ctx.fillStyle = "#ededee";
    ctx.fillText("phux / browser terminal", 16, 28);
    ctx.fillText("$ help", 16, 64);
    ctx.fillStyle = "#aaaab1";
    ctx.fillText("Split a pane. Run a command.", 16, 88);
    ctx.fillText("Each pane has its own terminal.", 16, 108);
    ctx.fillStyle = "#9ece6a";
    ctx.fillText(`Connected · ${count} pane(s)`, 16, 144);
    ctx.fillStyle = "#ededee";
    ctx.fillText("$ _", 16, 180);
    canvas.dataset.phuxPaneCount = String(count);
    canvas.dispatchEvent(new CustomEvent("phux-panes", { detail: { count, focused: String(focused), pending: false } }));
  }
  const client = {
    close() { if (!closed) { closed = true; log.closes++; } },
    free() { log.frees++; },
    resize(c, r) { canvas.width = c * 9; canvas.height = r * 16; canvas.style.width = `${c * 9}px`; canvas.style.height = `${r * 16}px`; paint(); },
    split_pane(axis) { count++; focused = count; log.actions.push(`split:${axis}`); paint(); },
    focus_next_pane() { focused = focused % count + 1; log.actions.push("next"); paint(); },
    close_pane() { count--; focused = 1; log.actions.push("close-pane"); paint(); },
    send_key(key) { log.actions.push(`key:${key}`); },
    set_ctrl_latch(held) { log.actions.push(`ctrl:${held}`); canvas.dispatchEvent(new CustomEvent("phux-modifiers", { detail: { ctrl: held } })); },
  };
  if (scenario === "rejected") {
    onEvent({ type: "close", code: 4001, category: "capacity", wasClean: true });
    return client;
  }
  const native = log.mode === "native" && scenario !== "fallback";
  onEvent({ type: "phux.session.v1", outcome: "accepted", backend: native ? "native" : "edge", expiresAt: Date.now() + 300_000,
    ...(scenario === "fallback" ? { fallbackReason: "native-capacity" } : {}) });
  paint();
  return client;
}
