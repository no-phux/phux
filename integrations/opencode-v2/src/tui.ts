import { Plugin } from "@opencode/plugin/tui";
import { createEffect } from "solid-js";

import { PhuxCli } from "../../runtime/src/adapter.js";
import { ActivePane } from "./active-pane.js";
import { OpenCodeLifecycle } from "./lifecycle.js";
import { parentPane } from "./parent.js";

/** Pane identity belongs to the TUI process, never to the shared OpenCode service. */
export default Plugin.define({
  id: "phux.tui",
  setup(ctx) {
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
      await active.dispose();
    };
  },
});
