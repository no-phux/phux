import type { OpenCodeLifecycleState } from "./lifecycle.js";

export interface PaneLifecycle {
  observeState(sessionID: string, state: OpenCodeLifecycleState): Promise<void>;
  ask(sessionID: string): Promise<void>;
  deleteSession(sessionID: string): Promise<void>;
  dispose(): Promise<void>;
}

/** Only the visible session may claim the terminal occupied by this TUI. */
export class ActivePane {
  private sessionID: string | undefined;
  private state: OpenCodeLifecycleState | undefined;

  constructor(private readonly lifecycle: PaneLifecycle) {}

  show(sessionID: string | undefined, state: OpenCodeLifecycleState = "idle"): void {
    if (sessionID !== this.sessionID) {
      if (this.sessionID !== undefined) void this.lifecycle.deleteSession(this.sessionID);
      this.sessionID = sessionID;
      this.state = undefined;
    }
    if (sessionID === undefined || this.state === state) return;
    this.state = state;
    void this.lifecycle.observeState(sessionID, state);
  }

  status(sessionID: string, state: OpenCodeLifecycleState): void {
    if (sessionID === this.sessionID) this.show(sessionID, state);
  }

  ask(sessionID: string): void {
    if (sessionID === this.sessionID) void this.lifecycle.ask(sessionID);
  }

  async dispose(): Promise<void> {
    this.show(undefined);
    await this.lifecycle.dispose();
  }
}
