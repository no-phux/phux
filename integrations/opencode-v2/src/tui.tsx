/** @jsxImportSource @opentui/solid */
import { Plugin, usePlugin } from "@opencode/plugin/tui";
import { createEffect, createSignal, For, onCleanup, onMount, Show } from "solid-js";

import { PhuxCli } from "../../runtime/src/adapter.js";
import { ActivePane } from "./active-pane.js";
import { OpenCodeLifecycle } from "./lifecycle.js";
import { parentPane } from "./parent.js";

/** Pane identity belongs to the TUI process, never to the shared OpenCode service. */
export default Plugin.define({
  id: "phux.tui",
  setup(ctx) {
    if (process.env.PHUX_AGENT_REPORTING === "0") return;
    const parent = parentPane(process.env.PHUX_TERMINAL_ID);
    if (parent === null) return;

    const lifecycle = new OpenCodeLifecycle({
      cli: new PhuxCli({ env: process.env }),
      target: () => parent,
      onError: (error) => console.error("phux OpenCode presence:", error),
    });
    const active = new ActivePane(lifecycle);
    const removeSlot = ctx.ui.slot({
      append: "app",
      render: () => {
        createEffect(() => {
          const route = ctx.ui.router.current();
          const sessionID = route.type === "session" ? route.sessionID : undefined;
          active.show(sessionID, sessionID !== undefined && ctx.data.session.status(sessionID) === "running"
            ? "working" : "idle");
        });
        return null!;
      },
    });
    // Show this session's phux-spawned terminals in the left sidebar so they
    // stay grouped with the session that owns them instead of becoming an
    // undifferentiated wall in the pane inventory.
    const removeSidebar = ctx.ui.slot({
      append: "sidebar.content",
      render: (input) => <PhuxSidebar sessionID={input.sessionID} />,
    });
    const stopStatus = ctx.data.on("session.status", (event) => {
      const { sessionID, status } = event.data;
      if (status.type === "busy") active.status(sessionID, "working");
      if (status.type === "idle") active.status(sessionID, "idle");
    });
    const stopIdle = ctx.data.on("session.idle", (event) => active.status(event.data.sessionID, "idle"));
    const stopPermission = ctx.data.on("permission.asked", (event) => active.ask(event.data.sessionID));

    return async () => {
      stopStatus();
      stopIdle();
      stopPermission();
      removeSlot();
      removeSidebar();
      await active.dispose();
    };
  },
});

function ownedTargets(ctx: any, sessionID: string): string[] {
  const found = new Set<string>();
  let messages: any[] = [];
  try {
    messages = ctx.data.session.message.list(sessionID) ?? [];
  } catch {
    messages = [];
  }
  for (const message of messages) {
    if (message?.type !== "assistant") continue;
    for (const part of message.content ?? []) {
      if (part?.type !== "tool" || part?.name !== "phux_spawn") continue;
      const state = part.state;
      if (state?.status !== "completed" && state?.status !== "running") continue;
      const target = state?.metadata?.target;
      if (typeof target === "string" && target.trim().length > 0) found.add(target);
      else {
        // Fall back to scanning the rendered text for a target selector.
        const text = (state?.content ?? []).map((c: any) => c?.text ?? "").join(" ");
        const match = text.match(/target\s*=\s*([@/\w-]+)/);
        if (match) found.add(match[1]);
      }
    }
  }
  return [...found];
}

function PhuxSidebar(props: { sessionID: string }) {
  const ctx = usePlugin();
  const [targets, setTargets] = createSignal<string[]>([]);
  const refresh = () => setTargets(ownedTargets(ctx, props.sessionID));
  onMount(() => {
    refresh();
    const timer = setInterval(refresh, 2000);
    onCleanup(() => clearInterval(timer));
  });
  return (
    <Show when={targets().length > 0}>
      <box flexDirection="column" marginTop={1}>
        <text>Phux terminals</text>
        <For each={targets()}>
          {(target) => <text>{target}</text>}
        </For>
      </box>
    </Show>
  );
}

// createEffect imported above.