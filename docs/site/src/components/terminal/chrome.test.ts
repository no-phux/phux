import { describe, expect, test } from "bun:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { PaneControls, SessionControls, TerminalHeader, TerminalInfo, TerminalOverlay, type TerminalPhase } from "../TerminalChrome";

const noop = () => {};
const overlay = { native: false, configured: true, authMessage: "", message: "The shell stopped safely.", warm: noop, launch: noop, beginAuth: noop, release: noop };

describe("terminal presentation", () => {
  test.each([
    ["checking", "Checking access…"], ["unlock", "A Linux shell, right here."],
    ["idle", "Launch edge WASM shell"], ["connecting", "Opening your terminal…"],
    ["closed", "The shell stopped safely."], ["error", "Return to controls"],
  ] as const)("%s has an actionable state panel", (phase, text) => {
    expect(renderToStaticMarkup(createElement(TerminalOverlay, { ...overlay, phase }))).toContain(text);
  });
  test("live removes the overlay rather than obscuring terminal input", () => {
    expect(renderToStaticMarkup(createElement(TerminalOverlay, { ...overlay, phase: "live" }))).toBe("");
  });
  test("unconfigured preview is honest and has no launch button", () => {
    const html = renderToStaticMarkup(createElement(TerminalOverlay, { ...overlay, phase: "idle", configured: false }));
    expect(html).toContain("Live demo is not configured.");
    expect(html).not.toContain("<button");
  });
  test("native sign-in keeps both providers, edge escape and limitations", () => {
    const html = renderToStaticMarkup(createElement(TerminalOverlay, { ...overlay, phase: "unlock", native: true, authMessage: "Sign-in failed safely." }));
    for (const text of ["Continue with GitHub", "Continue with Google", "Use instant edge shell", "five-minute", "No outbound network", "Open standalone shell", 'role="alert"']) expect(html).toContain(text);
  });
  test("every phase has a human-readable status", () => {
    const phases: TerminalPhase[] = ["idle", "checking", "unlock", "connecting", "live", "closed", "error"];
    for (const phase of phases) {
      const html = renderToStaticMarkup(createElement(TerminalHeader, { phase, timeLeft: null }));
      expect(html).toContain('aria-live="polite"');
      expect(html).not.toContain("undefined");
    }
  });
  test("pane cap disables both splits while navigation remains available", () => {
    const html = renderToStaticMarkup(createElement(PaneControls, { panes: { count: 4, focused: "1", pending: false }, error: "", splitVertical: noop, splitHorizontal: noop, nextPane: noop, closePane: noop }));
    expect(html).toContain('aria-label="Split left/right" title="Split left/right" disabled=""');
    expect(html).toContain('aria-label="Split top/bottom" title="Split top/bottom" disabled=""');
    expect(html).toContain("Next pane");
    expect(html).toContain("Close pane");
  });
  test("pending attach exposes cancellation and the disposable constraint", () => {
    const html = renderToStaticMarkup(createElement(SessionControls, { phase: "connecting", identity: null, release: noop, logout: noop }));
    expect(html).toContain("Cancel launch");
    expect(html).toContain("Disposable session · no network");
    expect(html).not.toContain("Sign out");
  });
  test("released sessions do not report a stale focused pane", () => {
    const html = renderToStaticMarkup(createElement(TerminalInfo, { native: false, identity: null, startupMs: null, live: false, focused: "2" }));
    expect(html).not.toContain("Focused terminal");
  });
  test("Info reports actual edge fallback, not the requested Linux runtime", () => {
    const html = renderToStaticMarkup(createElement(TerminalInfo, { native: true, backend: "edge", identity: null, startupMs: null, live: true, focused: "1" }));
    expect(html).toContain("Edge demo");
    expect(html).toContain("Limited demo commands, not a native Linux toolchain.");
    expect(html).toContain("Closing this demo releases its session");
    expect(html).toContain("<details");
    expect(html).not.toContain("<details open");
  });
});
