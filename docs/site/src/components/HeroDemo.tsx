import { useEffect, useId, useRef, useState } from "react";
import type PhuxTerminal from "./PhuxTerminal";
import Tour from "./tour/Tour";
import { SITE, docsHref } from "../lib/site";
import type { SessionPair } from "./terminal/core";
import "./HeroDemo.css";

type Mode = "demo" | "native";

function HeroTerminalActions({ mode, changeMode, close }: {
  mode: Mode; changeMode: (mode: Mode) => void; close: () => void;
}) {
  return <>
    <div className="mux-mode-row" role="group" aria-label="Terminal runtime">
      <button type="button" aria-pressed={mode === "demo"} onClick={() => changeMode("demo")}>Demo</button>
      <button type="button" aria-pressed={mode === "native"} onClick={() => changeMode("native")}>Linux shell</button>
    </div>
    <button type="button" className="mux-close" onClick={close} aria-label="Close live terminal">
      <span aria-hidden="true">×</span>
    </button>
  </>;
}

function TerminalDownload({ failed, retry }: { failed: boolean; retry: () => void }) {
  return <div className="mux-loading" role="status">
    {failed ? <>
      <p>The terminal client could not download.</p>
      <button type="button" onClick={retry}>Retry download</button>
    </> : "Loading the terminal client…"}
  </div>;
}

/** Where the demo Worker answers about one pairing code. */
function pairStatusUrl(wsUrl: string): string | null {
  try {
    const endpoint = new URL(wsUrl);
    if (endpoint.protocol !== "wss:" && endpoint.protocol !== "ws:") return null;
    endpoint.protocol = endpoint.protocol === "wss:" ? "https:" : "http:";
    endpoint.pathname = "/session/pair";
    endpoint.search = "";
    return endpoint.toString();
  } catch {
    return null;
  }
}

/**
 * Pair a phone with this session: the same shell on a second screen, no install
 * and no sign-in on the phone. The link goes in the URL fragment so the code
 * never reaches a server log or a referrer, and it is single-use.
 */
function PairPanel({ wsUrl, pair }: { wsUrl: string; pair: SessionPair }) {
  const [open, setOpen] = useState(false);
  const [linked, setLinked] = useState(false);
  const [copied, setCopied] = useState(false);
  const [qr, setQr] = useState("");
  const [expired, setExpired] = useState(() => Date.now() > pair.expiresAt);
  const link = `${window.location.origin}/phone#${pair.code}`;

  // A pairing code lives five minutes. Say so instead of offering a QR that
  // would be refused: the shell is disposable, so reloading for a fresh one is
  // the honest recovery.
  useEffect(() => {
    if (expired) return;
    const timer = setTimeout(() => setExpired(true), Math.max(0, pair.expiresAt - Date.now()));
    return () => clearTimeout(timer);
  }, [expired, pair.expiresAt]);

  // The QR encoder is only fetched when someone actually asks to pair, so the
  // landing's first paint never pays for it.
  useEffect(() => {
    if (!open || qr) return;
    let active = true;
    void import("uqr").then(({ renderSVG }) => {
      if (active) setQr(renderSVG(link, { ecc: "M", border: 1 }));
    });
    return () => {
      active = false;
    };
  }, [open, qr, link]);

  useEffect(() => {
    if (!open || linked) return;
    const status = pairStatusUrl(wsUrl);
    if (!status) return;
    const probe = new URL(status);
    probe.searchParams.set("code", pair.code);
    let active = true;
    const poll = async () => {
      try {
        const response = await fetch(probe, { headers: { Accept: "application/json" } });
        if (!response.ok) return;
        const value = (await response.json()) as { linked?: unknown };
        if (active && value.linked === true) setLinked(true);
      } catch {
        // A failed poll is not a failed pairing; the next tick tries again.
      }
    };
    void poll();
    const timer = setInterval(poll, 1500);
    return () => {
      active = false;
      clearInterval(timer);
    };
  }, [open, linked, pair.code, wsUrl]);

  return (
    <div className="mux-pair">
      <button
        type="button"
        className="mux-pair-toggle"
        aria-expanded={open}
        onClick={() => setOpen((current) => !current)}
      >
        {linked ? "Phone linked" : "Pair your phone"}<span aria-hidden="true"> ↗</span>
      </button>
      {open && (
        <div className="mux-pair-body">
          {expired && !linked ? (
            <p className="mux-pair-text">
              <strong>This pairing code expired.</strong> Reload the page for a fresh shell and a
              fresh code.
            </p>
          ) : (
            <>
              <span className="mux-pair-code" aria-hidden="true" dangerouslySetInnerHTML={{ __html: qr }} />
              <div className="mux-pair-text">
                <p>
                  <strong>Point your phone camera here.</strong> The same shell opens in its browser —
                  no install, no sign-in.
                </p>
                <p className="mux-pair-state" role="status">
                  {linked ? "Linked. Both screens drive this shell." : "Waiting for your phone…"}
                </p>
                <button
                  type="button"
                  className="mux-pair-copy"
                  onClick={() => {
                    void navigator.clipboard.writeText(link).then(() => {
                      setCopied(true);
                      setTimeout(() => setCopied(false), 1400);
                    }).catch(() => {
                      // Clipboard refusal leaves the link visible in the QR.
                    });
                  }}
                >
                  {copied ? "Link copied" : "Copy link"}
                </button>
              </div>
            </>
          )}
        </div>
      )}
    </div>
  );
}

export default function HeroDemo({ wsUrl = "" }: { wsUrl?: string }) {
  const [open, setOpen] = useState(false);
  const [mode, setMode] = useState<Mode>("demo");
  const [Terminal, setTerminal] = useState<typeof PhuxTerminal | null>(null);
  const [loadFailed, setLoadFailed] = useState(false);
  const [authError, setAuthError] = useState("");
  const [pair, setPair] = useState<SessionPair | null>(null);
  const dialog = useRef<HTMLDialogElement>(null);
  const opener = useRef<HTMLElement | null>(null);
  const loading = useRef<Promise<void> | null>(null);
  const id = useId();

  function loadTerminal() {
    if (!loading.current) {
      setLoadFailed(false);
      loading.current = import("./PhuxTerminal")
        .then((module) => setTerminal(() => module.default))
        .catch(() => {
          loading.current = null;
          setLoadFailed(true);
        });
    }
  }

  function launch() {
    opener.current =
      document.activeElement instanceof HTMLElement
        ? document.activeElement
        : null;
    setMode("demo");
    setOpen(true);
    loadTerminal();
  }

  // Any [data-demo-launch] link on the page opens the dialog; without
  // JavaScript it falls through to the standalone shell at its href.
  useEffect(() => {
    function onClick(event: MouseEvent) {
      if (event.defaultPrevented || event.button !== 0) return;
      if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
      const target = event.target instanceof Element ? event.target : null;
      if (!target?.closest("[data-demo-launch]")) return;
      event.preventDefault();
      launch();
    }
    document.addEventListener("click", onClick);
    return () => document.removeEventListener("click", onClick);
  }, []);

  useEffect(() => {
    const result = document.documentElement.dataset.nativeAuthResult;
    if (!result) return;
    delete document.documentElement.dataset.nativeAuthResult;
    setAuthError(
      result === "error"
        ? (document.documentElement.dataset.nativeAuthError ??
            "Sign-in did not finish. Try a provider again below, or choose the edge tour.")
        : "",
    );
    delete document.documentElement.dataset.nativeAuthError;
    setMode("native");
    setOpen(true);
    loadTerminal();
  }, []);

  useEffect(() => {
    if (!open) return;
    const element = dialog.current;
    if (!element) return;
    element.showModal();
    const overflow = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    return () => {
      element.close();
      document.body.style.overflow = overflow;
      opener.current?.focus({ preventScroll: true });
    };
  }, [open]);

  return (
    <div className="hero-demo">
      <Tour prepare={loadTerminal} />
      <noscript>
        <p>
          <a href="/embed">Open the hosted shell</a> or{" "}
          <a href={docsHref("/quickstart")}>start locally</a>.
        </p>
      </noscript>
      <dialog
        ref={dialog}
        className="mux-dialog terminal-surface"
        aria-labelledby={`${id}-title`}
        aria-describedby={`${id}-description`}
        onCancel={(event) => {
          event.preventDefault();
          setOpen(false);
        }}
        onClose={() => setOpen(false)}
      >
        <h2 className="mux-sr-only" id={`${id}-title`}>{SITE.name} terminal</h2>
        {!Terminal && <header className="mux-dialog-head">
          <HeroTerminalActions mode={mode} changeMode={(next) => { setAuthError(""); setMode(next); }} close={() => setOpen(false)} />
        </header>}
        {authError && (
          <p className="mux-dialog-description" role="alert">
            {authError}
          </p>
        )}
        <p className="mux-sr-only" id={`${id}-description`}>
          {mode === "demo" ? (
            <>A limited demo shell, no sign-in. Try <code>help</code> or <code>demo</code>, then split a pane.</>
          ) : (
            <>A disposable Linux shell. Sign-in required; limited capacity with an edge fallback.</>
          )}
        </p>
        {open &&
          (Terminal ? (
            <Terminal
              key={mode}
              wsUrl={wsUrl}
              mode={mode}
              cols={110}
              rows={24}
              autoStart
              focusOnStart
              headerActions={<HeroTerminalActions mode={mode} changeMode={(next) => { setAuthError(""); setMode(next); }} close={() => setOpen(false)} />}
              onSession={(session) => setPair(session.pair ?? null)}
            />
          ) : (
            <TerminalDownload failed={loadFailed} retry={loadTerminal} />
          ))}
        {open && pair && <PairPanel wsUrl={wsUrl} pair={pair} />}
      </dialog>
    </div>
  );
}
