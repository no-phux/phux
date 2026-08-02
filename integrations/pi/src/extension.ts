import type {
  ExtensionAPI,
  ExtensionContext,
} from "@earendil-works/pi-coding-agent";

import { PhuxCli } from "./adapter.js";
import {
  PHUX_CONTEXT_CUSTOM_TYPE,
  PhuxContextAwareness,
  contextAwarenessEnabled,
  type PhuxContextEmission,
  normalizeTerminalIdentity,
} from "./awareness.js";
import { PhuxTargetPicker } from "./components.js";
import { registerPhuxLifecycle } from "./lifecycle.js";
import type { AgentPane } from "./schemas.js";
import { registerPhuxTools } from "./tools.js";
import {
  PhuxTargetStore,
  formatTargetStatus,
  type PhuxTargetSnapshot,
} from "./target-store.js";

export interface PhuxExtensionOptions {
  readonly cli?: PhuxCli;
  readonly contextAwareness?: PhuxContextAwareness;
  readonly env?: NodeJS.ProcessEnv;
}

/** Register commands and lifecycle hooks around the extension's single shared target store. */
export function registerPhuxExtension(
  pi: ExtensionAPI,
  options: PhuxExtensionOptions = {},
): PhuxTargetStore {
  const cli = options.cli ?? new PhuxCli();
  const environment = options.env ?? process.env;
  const store = new PhuxTargetStore(pi, cli);
  const awareness = options.contextAwareness ?? new PhuxContextAwareness(cli, {
    enabled: contextAwarenessEnabled(environment.PHUX_CONTEXT_AWARENESS),
  });
  const contextIdentity = () => {
    const self = normalizeTerminalIdentity(environment.PHUX_TERMINAL_ID);
    const selected = store.snapshot.selection?.selector;
    return {
      ...(self === null ? {} : { self }),
      ...(selected === undefined ? {} : { selected }),
    };
  };
  registerPhuxTools(pi, cli, store);

  const updateStatus = (ctx: ExtensionContext): void => {
    if (!ctx.hasUI) return;
    ctx.ui.setStatus("phux", formatTargetStatus(store.snapshot));
  };

  const restore = async (ctx: ExtensionContext): Promise<void> => {
    store.restoreFromBranch(ctx.sessionManager.getBranch());
    await store.refresh(ctx.signal);
    updateStatus(ctx);
  };

  const contextMessage = (emission: PhuxContextEmission) => ({
    customType: PHUX_CONTEXT_CUSTOM_TYPE,
    content: emission.text,
    display: false,
    details: { version: emission.version, kind: emission.kind, seq: emission.seq },
  });
  const appendCheckpoint = async (ctx: ExtensionContext): Promise<void> => {
    const sessionId = ctx.sessionManager.getSessionId();
    awareness.forceCheckpoint(sessionId);
    const emission = await awareness.next(sessionId, contextIdentity(), ctx.signal);
    if (emission !== null) pi.sendMessage(contextMessage(emission));
  };

  pi.on("session_start", async (_event, ctx) => restore(ctx));
  pi.on("session_tree", async (_event, ctx) => {
    await restore(ctx);
    await appendCheckpoint(ctx);
  });
  pi.on("before_agent_start", async (_event, ctx) => {
    const emission = await awareness.next(
      ctx.sessionManager.getSessionId(),
      contextIdentity(),
      ctx.signal,
    );
    if (emission === null) return;
    return { message: contextMessage(emission) };
  });
  pi.on("session_compact", async (_event, ctx) => appendCheckpoint(ctx));

  pi.registerCommand("phux", {
    description: "Select the default phux pane target",
    handler: async (_args, ctx) => {
      if (!ctx.hasUI) return;
      const snapshot = await store.refresh(ctx.signal);
      updateStatus(ctx);
      if (snapshot.availability === "unavailable") {
        ctx.ui.notify(`Cannot inventory phux panes: ${snapshot.reason ?? "phux is unavailable"}`, "error");
        return;
      }
      if (store.panes.length === 0) {
        ctx.ui.notify("No phux panes are available; the current target was not changed", "warning");
        return;
      }

      const selected = await ctx.ui.custom<AgentPane | null>((tui, theme, _keybindings, done) =>
        new PhuxTargetPicker(store.panes, theme, done, () => done(null), () => tui.requestRender()));
      if (selected == null) return;
      const selection = store.select(selected);
      updateStatus(ctx);
      ctx.ui.notify(`phux target: ${selection.display}`, "info");
    },
  });

  pi.registerCommand("phux-status", {
    description: "Inspect the selected phux target",
    handler: async (_args, ctx) => {
      await store.refresh(ctx.signal);
      updateStatus(ctx);
      if (ctx.hasUI) {
        ctx.ui.notify(formatDetailedStatus(store.snapshot), statusNotice(store.snapshot));
      }
    },
  });

  pi.registerCommand("phux-attach", {
    description: "Show a safe human attach handoff for the selected phux session",
    handler: async (_args, ctx) => {
      if (!ctx.hasUI) return;
      const selection = store.snapshot.selection;
      if (selection === null) {
        ctx.ui.notify("No phux target selected. Run /phux first.", "warning");
        return;
      }
      ctx.ui.notify(formatAttachHandoff(store.snapshot), "info");
    },
  });

  registerPhuxLifecycle(pi, store, {
    cli,
    onError: (error) => {
      const message = error instanceof Error ? error.message : String(error);
      console.warn(`[phux] lifecycle: ${message}`);
    },
  });

  return store;
}


export function formatDetailedStatus(snapshot: PhuxTargetSnapshot): string {
  const base = formatTargetStatus(snapshot);
  return snapshot.reason === undefined ? base : `${base}\n${snapshot.reason}`;
}

export function formatAttachHandoff(snapshot: PhuxTargetSnapshot): string {
  const selection = snapshot.selection;
  if (selection === null) return "No phux target selected.";
  const argv = JSON.stringify(["phux", "attach", selection.session]);
  const warning = snapshot.availability === "available"
    ? ""
    : ` Target is currently ${snapshot.availability}; the saved session is shown without fallback.`;
  return `Run outside Pi using argv ${argv}. Then navigate in phux to pane ${selection.selector} (window ${selection.window}). The extension does not execute attach.${warning}`;
}

function statusNotice(snapshot: PhuxTargetSnapshot): "info" | "warning" | "error" {
  if (snapshot.availability === "unavailable") return "error";
  if (snapshot.availability === "stale" || snapshot.availability === "unselected") return "warning";
  return "info";
}
