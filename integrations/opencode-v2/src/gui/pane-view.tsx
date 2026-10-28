import { createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show, type JSX } from "solid-js";
import { usePanel, usePlugin, type MountedSession, type SessionScreen } from "@opencode/plugin/gui";

import { errorMessage, type PhuxClient } from "./client.js";
import type { definition } from "./definition.js";
import { isAgent, paneKey, paneName, stateKey, type Fleet } from "./fleet.js";
import {
  batchKeys,
  keyAction,
  parseFormatterHtml,
  plainLines,
  reuseLines,
  screenText,
  shortPath,
  withCaret,
  type KeyAction,
  type Line,
} from "./terminal.js";
import type { Screen } from "../rpc.js";

const HISTORY_ROWS = 500;
const QUOTE_ROWS = 80;
const MIN_FRAME_MS = 120;
const WAIT_SECONDS = 10;

export interface PaneViewProps {
  readonly target: string;
  readonly session: MountedSession;
  readonly screen: SessionScreen;
  readonly fleet: Fleet;
  readonly client: PhuxClient;
}

type Status = "loading" | "live" | "closed" | "error";

/**
 * A live view of one phux pane. It long-polls the server: each call waits
 * for the pane to change after the last cursor and returns the server's own
 * styled rendering, so an idle pane costs nothing and a busy one repaints at
 * most every MIN_FRAME_MS.
 */
export function PaneView(props: PaneViewProps): JSX.Element {
  const ctx = usePlugin<typeof definition>();
  const panel = usePanel();
  const ui = ctx.ui;
  const [lines, setLines] = createSignal<Line[]>([]);
  const [status, setStatus] = createSignal<Status>("loading");
  const [error, setError] = createSignal<string>();
  const [title, setTitle] = createSignal<string | null>(null);
  const [cols, setCols] = createSignal(80);
  const [typing, setTyping] = createSignal(false);
  const [pinned, setPinned] = createSignal(true);
  const [width, setWidth] = createSignal(0);
  const [draft, setDraft] = createSignal("");
  const [sending, setSending] = createSignal(false);
  const [retry, setRetry] = createSignal(0);
  let scroller: HTMLDivElement | undefined;
  let charWidth = 0.6;

  const pane = () => props.fleet.pane(props.target);
  const owned = () => props.fleet.owned(props.session.id).includes(props.target);
  const name = () => {
    const known = pane();
    return known === undefined ? (title() ?? props.target) : paneName(known);
  };
  const agent = () => {
    const known = pane();
    return known !== undefined && isAgent(known) ? known : undefined;
  };
  const shownState = () => (status() === "closed" ? "closed" : (pane()?.state ?? "unknown"));
  const fontSize = createMemo(() => {
    if (!ctx.stores.prefs.value.fitWidth || width() === 0) return 12;
    return Math.max(8, Math.min(12, Math.floor(((width() - 22) / (cols() * charWidth)) * 10) / 10));
  });

  const atBottom = () => scroller === undefined || scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 6;
  const toBottom = () => {
    if (scroller !== undefined) scroller.scrollTop = scroller.scrollHeight;
  };

  const show = (screen: Screen): void => {
    const parsed = screen.html !== undefined ? parseFormatterHtml(screen.html) : plainLines(screen.lines ?? []);
    const caret = screen.caret?.visible === true ? screen.caret : null;
    const follow = pinned() || atBottom();
    setLines((previous) => reuseLines(previous, withCaret(parsed, screen.historyRows ?? 0, screen.rows ?? 0, caret)));
    if (screen.title !== undefined) setTitle(screen.title);
    if (screen.cols !== undefined && screen.cols > 0) setCols(screen.cols);
    setStatus("live");
    setError(undefined);
    if (follow) requestAnimationFrame(toBottom);
  };

  const run = async (target: string, signal: AbortSignal): Promise<void> => {
    let cursor: string | undefined;
    let known: string | undefined;
    // phux sends one `dirty` per burst of output, so while a pane streams the
    // view polls every frame without waiting, until two renders match. After
    // that it waits briefly, so a change that raced the watch shows within a
    // second, and a quiet pane backs off to WAIT_SECONDS.
    let wait = 0;
    let streaming = false;
    let settled = 0;
    let failures = 0;
    while (!signal.aborted) {
      const started = Date.now();
      try {
        const screen = await props.client.screen(
          {
            target,
            history: HISTORY_ROWS,
            waitSeconds: wait,
            ...(cursor === undefined ? {} : { after: cursor }),
            ...(known === undefined ? {} : { known }),
          },
          { signal, directory: props.session.directory },
        );
        if (signal.aborted) return;
        failures = 0;
        cursor = screen.cursor ?? cursor;
        if (screen.closed) {
          setStatus("closed");
          return;
        }
        if (screen.event === "dirty") streaming = true;
        if (screen.changed) {
          known = screen.hash;
          show(screen);
          settled = 0;
        } else if (streaming && ++settled >= 2) {
          streaming = false;
        }
        wait = streaming ? 0 : screen.changed ? 1 : Math.min(WAIT_SECONDS, Math.max(1, wait * 2));
      } catch (cause) {
        if (signal.aborted) return;
        failures += 1;
        setError(errorMessage(cause));
        if (status() !== "live" || failures > 2) setStatus("error");
        await sleep(Math.min(10_000, 500 * 2 ** failures), signal);
        continue;
      }
      const elapsed = Date.now() - started;
      if (elapsed < MIN_FRAME_MS) await sleep(MIN_FRAME_MS - elapsed, signal);
    }
  };

  createEffect(on([() => props.target, () => panel.visible(), retry], ([target, visible]) => {
    if (!visible) return;
    const controller = new AbortController();
    onCleanup(() => controller.abort());
    void run(target, controller.signal);
  }));

  onMount(() => {
    charWidth = measureCharWidth(ctx.appearance.font("mono"));
  });
  // The screen mounts only once the first render arrives, so observe it from its ref.
  const observer = new ResizeObserver(([entry]) => setWidth(entry?.contentRect.width ?? 0));
  onCleanup(() => observer.disconnect());
  const attachScreen = (element: HTMLDivElement): void => {
    scroller = element;
    observer.disconnect();
    observer.observe(element);
  };

  // Keystrokes queue in order and leave in batches; calls never overlap.
  let pending: KeyAction[] = [];
  let flushing: Promise<void> = Promise.resolve();
  let flushQueued = false;
  const deliver = (input: { keys?: string[]; text?: string; submit?: boolean }): Promise<void> => {
    flushing = flushing
      .then(() => props.client.input({ target: props.target, ...input }, { directory: props.session.directory }))
      .then(
        () => undefined,
        (cause: unknown) => {
          ctx.toast.show({ variant: "error", title: ctx.t("pane.send.failed"), description: errorMessage(cause) });
        },
      );
    return flushing;
  };
  const flushKeys = (): void => {
    const calls = batchKeys(pending);
    pending = [];
    for (const keys of calls) void deliver({ keys });
  };
  const queue = (action: KeyAction): void => {
    if (status() !== "live") return;
    pending.push(action);
    if (flushQueued) return;
    flushQueued = true;
    setTimeout(() => {
      flushQueued = false;
      flushKeys();
    }, 0);
  };
  /** Paste and submit go behind keys already typed, in the same ordered chain. */
  const after = (input: { text: string; submit?: boolean }): Promise<void> => {
    flushKeys();
    return deliver(input);
  };
  const platform = typeof navigator !== "undefined" && /Mac/i.test(navigator.platform) ? "mac" : "other";

  const onKeyDown = (event: KeyboardEvent): void => {
    if (event.isComposing || status() !== "live") return;
    const action = keyAction(event, platform);
    if (action === undefined) return;
    event.preventDefault();
    event.stopPropagation();
    setPinned(true);
    queue(action);
  };
  const onPaste = (event: ClipboardEvent): void => {
    const text = event.clipboardData?.getData("text/plain");
    if (text === undefined || text.length === 0) return;
    event.preventDefault();
    if (status() === "live") void after({ text });
  };
  const onScroll = (): void => {
    setPinned(atBottom());
  };

  const submit = async (value: string): Promise<void> => {
    const text = value.trimEnd();
    if (text.trim().length === 0 || sending()) return;
    setSending(true);
    try {
      if (agent() !== undefined) {
        flushKeys();
        await flushing;
        await props.client.prompt({ target: props.target, text }, { directory: props.session.directory });
      } else {
        await after({ text, submit: true });
      }
      setDraft("");
      setPinned(true);
      void props.fleet.refresh();
    } catch (cause) {
      ctx.toast.show({ variant: "error", title: ctx.t("pane.send.failed"), description: errorMessage(cause) });
    } finally {
      setSending(false);
    }
  };

  const selectionText = (): string | undefined => {
    const selection = window.getSelection();
    if (selection === null || selection.isCollapsed || scroller === undefined) return undefined;
    if (!scroller.contains(selection.anchorNode) || !scroller.contains(selection.focusNode)) return undefined;
    const text = selection.toString();
    return text.trim().length === 0 ? undefined : text;
  };

  const quote = (): void => {
    const composer = props.screen.composer;
    if (!composer.quotable()) return;
    const text = selectionText() ?? screenText(lines(), QUOTE_ROWS);
    if (text.length === 0) return;
    const commentID = crypto.randomUUID();
    composer.attach({
      type: "quote",
      commentID,
      text,
      source: {
        label: ctx.t("pane.source", { target: props.target }),
        icon: "console",
        subject: ctx.t("pane.subject", { target: props.target, title: name() }),
      },
    });
    composer.focus(commentID);
  };

  const copy = async (): Promise<void> => {
    await ctx.system.copy(selectionText() ?? screenText(lines(), Number.MAX_SAFE_INTEGER));
    ctx.toast.show({ variant: "success", title: ctx.t("pane.copied") });
  };

  const attach = async (): Promise<void> => {
    const known = pane();
    const command = known === undefined ? "phux attach" : `phux attach ${shellWord(known.session)}`;
    await ctx.system.copy(command);
    ctx.toast.show({ variant: "success", title: ctx.t("pane.attach.copied", { command }) });
  };

  const kill = async (): Promise<void> => {
    const confirmed = await ctx.dialogs.confirm({
      title: ctx.t("pane.kill.confirm", { target: props.target }),
      description: ctx.t("pane.kill.description"),
      confirm: ctx.t("pane.kill"),
      danger: true,
    });
    if (!confirmed) return;
    try {
      await props.client.kill({ sessionID: props.session.id, target: props.target }, { directory: props.session.directory });
      void props.fleet.refresh();
      ctx.layout.close(paneKey(ctx, props.target), props.session);
    } catch (cause) {
      ctx.toast.show({ variant: "error", title: ctx.t("pane.kill"), description: errorMessage(cause) });
    }
  };

  const metaLine = () => {
    const known = pane();
    const parts = [props.target];
    if (known !== undefined) {
      parts.push(known.session);
      if (isAgent(known)) parts.push(known.agent.label);
      if (known.cwd !== null) parts.push(shortPath(known.cwd));
    }
    return parts.join(" · ");
  };

  const key = (label: string, spec: string) => (
    <button type="button" class="phux-key" onClick={() => queue({ kind: "key", spec })}>
      {label}
    </button>
  );

  return (
    <div class="phux-root" classList={{ "phux-dark": ctx.appearance.mode() === "dark", "phux-light": ctx.appearance.mode() === "light" }}>
      <div class="phux-header">
        <span class="phux-dot" data-state={shownState()} />
        <div class="phux-header-text">
          <span class="phux-header-title" title={name()}>{name()}</span>
          <span class="phux-header-meta" title={pane()?.cwd ?? ""}>
            {status() === "closed" || pane() === undefined ? ctx.t(`state.${shownState()}`) : ctx.t(stateKey(pane()!))} · {metaLine()}
          </span>
        </div>
        <div class="phux-header-actions">
          <ui.Tooltip value={ctx.t("pane.quote.selection")}>
            <ui.IconButton icon="outline-quote" label={ctx.t("pane.quote")} variant="ghost" size="small" disabled={lines().length === 0} onClick={quote} />
          </ui.Tooltip>
          <ui.Menu
            label={ctx.t("name")}
            icon="outline-dots"
            placement="bottom-end"
            items={[
              { label: ctx.t("pane.copy"), icon: "copy", onSelect: () => void copy() },
              { label: ctx.t("pane.attach"), icon: "console", onSelect: () => void attach() },
              {
                label: ctx.t("pane.fit"),
                icon: ctx.stores.prefs.value.fitWidth ? "check" : "expand",
                onSelect: () => ctx.stores.prefs.update((prefs) => {
                  prefs.fitWidth = !prefs.fitWidth;
                }),
              },
              ...(owned() ? [{ type: "separator" as const }, { label: ctx.t("pane.kill"), icon: "trash" as const, onSelect: () => void kill() }] : []),
            ]}
          />
        </div>
        <Show when={panel.reserve()}>
          <span class="phux-header-reserve" />
        </Show>
      </div>

      <Show
        when={status() !== "loading" || lines().length > 0}
        fallback={
          <div class="phux-empty">
            <ui.Loader size="large" />
            <span>{ctx.t("pane.loading", { target: props.target })}</span>
          </div>
        }
      >
        <Show
          when={status() !== "error" || lines().length > 0}
          fallback={
            <div class="phux-empty">
              <span class="phux-empty-title">{ctx.t("pane.error")}</span>
              <span class="phux-error">{error()}</span>
              <ui.Button size="small" variant="neutral" onClick={() => setRetry((count) => count + 1)}>
                {ctx.t("pane.retry")}
              </ui.Button>
            </div>
          }
        >
          <div class="phux-screen-wrap">
            <div
              ref={attachScreen}
              class="phux-screen"
              tabIndex={0}
              role="textbox"
              aria-label={ctx.t("pane.type")}
              aria-multiline="true"
              data-typing={typing() ? "true" : "false"}
              style={{ "font-family": ctx.appearance.font("mono"), "font-size": `${fontSize()}px` }}
              onFocus={() => setTyping(status() === "live")}
              onBlur={() => setTyping(false)}
              onKeyDown={onKeyDown}
              onPaste={onPaste}
              onScroll={onScroll}
            >
              <For each={lines()}>
                {(line) => (
                  <div class="phux-line">
                    <For each={line.segments}>
                      {(segment) => (
                        <span classList={{ "phux-caret": segment.caret === true }} style={segment.style}>
                          {segment.text}
                        </span>
                      )}
                    </For>
                  </div>
                )}
              </For>
            </div>
            <Show when={!pinned()}>
              <div class="phux-jump">
                <ui.Button
                  size="small"
                  variant="neutral"
                  icon="arrow-down-to-line"
                  onClick={() => {
                    setPinned(true);
                    toBottom();
                  }}
                >
                  {ctx.t("pane.bottom")}
                </ui.Button>
              </div>
            </Show>
          </div>
          <Show
            when={status() !== "closed"}
            fallback={<div class="phux-hint">{ctx.t("pane.closed")}</div>}
          >
            <div class="phux-hint">
              {typing() ? ctx.t("pane.type.on", { target: props.target }) : ctx.t("pane.type.hint")}
            </div>
            <div class="phux-input">
              <div class="phux-input-row">
                <ui.TextInput
                  value={draft()}
                  onInput={setDraft}
                  onSubmit={(value) => void submit(value)}
                  label={ctx.t("pane.send")}
                  placeholder={agent() === undefined
                    ? ctx.t("pane.send.placeholder", { target: props.target })
                    : ctx.t("pane.prompt.placeholder", { agent: paneName(agent()!) })}
                  disabled={sending()}
                />
                <ui.Tooltip value={ctx.t("pane.interrupt")}>
                  <ui.IconButton icon="stop" label={ctx.t("pane.interrupt")} variant="ghost" onClick={() => queue({ kind: "key", spec: "C-c" })} />
                </ui.Tooltip>
              </div>
              <div class="phux-keys">
                {key("esc", "Escape")}
                {key("tab", "Tab")}
                {key("↑", "Up")}
                {key("↓", "Down")}
                {key("←", "Left")}
                {key("→", "Right")}
                {key("⏎", "Enter")}
                {key("^C", "C-c")}
                {key("^D", "C-d")}
                {key("^R", "C-r")}
              </div>
            </div>
          </Show>
        </Show>
      </Show>
    </div>
  );
}

function sleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    if (signal.aborted) return resolve();
    const timer = setTimeout(done, ms);
    function done(): void {
      clearTimeout(timer);
      signal.removeEventListener("abort", done);
      resolve();
    }
    signal.addEventListener("abort", done, { once: true });
  });
}

/** Width of one monospace cell as a fraction of the font size. */
function measureCharWidth(family: string): number {
  const probe = document.createElement("span");
  probe.textContent = "0".repeat(100);
  probe.style.cssText = `position:absolute;visibility:hidden;white-space:pre;font-size:100px;font-family:${family}`;
  document.body.append(probe);
  const width = probe.getBoundingClientRect().width / 10_000;
  probe.remove();
  return width > 0.3 && width < 1 ? width : 0.6;
}

function shellWord(value: string): string {
  return /^[A-Za-z0-9_./:@%+=-]+$/.test(value) ? value : `'${value.replaceAll("'", "'\\''")}'`;
}
