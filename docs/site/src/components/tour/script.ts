// The landing tour is a scripted illustration, not a live session. Every
// chapter is a list of timed steps applied on top of the previous chapter's
// final state, so the story stays continuous and each frame is a pure
// function of (chapter, time).

export type NodeId = "server" | "terminal" | "desktop" | "phone" | "devbox";
export type ClientId = "terminal" | "desktop" | "phone";
export type PaneId = "shell" | "build" | "claude" | "train";
export type Status = "idle" | "working" | "blocked" | "done";
export type View = "off" | "attached" | "detached";
export type Tone = "cmd" | "dim" | "ok" | "ask" | "agent";
export type Line = { text: string; tone?: Tone; typing?: boolean };
export type Pane = { lines: Line[]; status: Status };
export type Signal = { from: NodeId; to: NodeId; label: string; progress: number };

export type Frame = {
  panes: Record<PaneId, Pane>;
  views: Record<ClientId, View>;
  /** Pane shown by the terminal and phone while attached. */
  focus: PaneId;
  /** The terminal is attached to the remote host instead of the local one. */
  remote: boolean;
  log: string;
  notice: string | null;
  signals: Signal[];
  tap: { key: string; progress: number } | null;
};

type Step = { at: number } & (
  | { line: string; pane: PaneId; tone?: Tone }
  | { type: string; pane: PaneId; prompt?: string; agent?: boolean }
  | { status: Status; pane: PaneId }
  | { view: View; client: ClientId }
  | { signal: string; from: NodeId; to: NodeId | readonly NodeId[] }
  | { log: string }
  | { notice: string | null }
  | { tap: string }
  | { focus: PaneId }
  | { remote: boolean }
);

export type Chapter = {
  id: string;
  label: string;
  title: string;
  text: string;
  href: string;
  seconds: number;
  steps: readonly Step[];
};

export const SIGNAL_SECONDS = 0.9;
const TAP_SECONDS = 0.6;
const TYPE_CPS = 24;
const VIEWS: readonly NodeId[] = ["terminal", "desktop", "phone"];

const BASE: Frame = {
  panes: {
    shell: { lines: [], status: "idle" },
    build: { lines: [], status: "idle" },
    claude: {
      lines: [
        { text: "$ claude", tone: "cmd" },
        { text: "✻ Ready. What should we work on?", tone: "dim" },
      ],
      status: "idle",
    },
    train: {
      lines: [
        { text: "$ python train.py", tone: "cmd" },
        { text: "epoch 11/50  loss 0.244", tone: "dim" },
      ],
      status: "working",
    },
  },
  views: { terminal: "off", desktop: "off", phone: "off" },
  focus: "build",
  remote: false,
  log: "",
  notice: null,
  signals: [],
  tap: null,
};

export const CHAPTERS: readonly Chapter[] = [
  {
    id: "attach",
    label: "Attach",
    title: "One terminal. Every screen.",
    text: "The server owns the shell. Your terminal, Phux.app, and your phone are views of the same running process.",
    href: "/concepts",
    seconds: 8.5,
    steps: [
      { at: 0.2, type: "phux attach api", pane: "shell" },
      { at: 1.0, signal: "attach", from: "terminal", to: "server" },
      { at: 1.4, view: "attached", client: "terminal" },
      { at: 1.6, type: "cargo build --release", pane: "build" },
      { at: 2.6, signal: "keys", from: "terminal", to: "server" },
      { at: 2.9, status: "working", pane: "build" },
      { at: 3.0, line: "   Compiling api v0.4.2", pane: "build", tone: "dim" },
      { at: 3.2, signal: "attach", from: "desktop", to: "server" },
      { at: 3.6, view: "attached", client: "desktop" },
      { at: 4.0, signal: "attach", from: "phone", to: "server" },
      { at: 4.4, view: "attached", client: "phone" },
      { at: 4.8, line: "   Compiling tokio v1.47.1", pane: "build", tone: "dim" },
      { at: 4.9, signal: "output", from: "server", to: VIEWS },
      { at: 6.2, line: "    Finished `release` in 41s", pane: "build", tone: "ok" },
      { at: 6.2, status: "done", pane: "build" },
      { at: 6.3, signal: "output", from: "server", to: VIEWS },
    ],
  },
  {
    id: "agents",
    label: "Agents",
    title: "Agents work in the open.",
    text: "Claude runs in a pane and drives other terminals through phux. Its state reaches every screen, so you see when it needs you.",
    href: "/consumers/getting-started",
    seconds: 9,
    steps: [
      { at: 0, focus: "claude" },
      { at: 0.3, type: "fix the flaky migration test", pane: "claude", prompt: "> " },
      { at: 1.6, signal: "keys", from: "terminal", to: "server" },
      { at: 1.9, status: "working", pane: "claude" },
      { at: 2.0, line: "Reading migrations/0042_users.sql", pane: "claude", tone: "dim" },
      { at: 2.8, log: "@7 → phux_run @3 cargo test" },
      { at: 2.9, type: "cargo test", pane: "build", agent: true },
      { at: 3.5, status: "working", pane: "build" },
      { at: 3.7, line: "running 128 tests", pane: "build", tone: "dim" },
      { at: 3.8, signal: "output", from: "server", to: VIEWS },
      { at: 4.9, line: "test result: ok. 128 passed", pane: "build", tone: "ok" },
      { at: 4.9, status: "done", pane: "build" },
      { at: 5.6, line: "Tests pass. Apply 0042 to staging? (y/n)", pane: "claude", tone: "ask" },
      { at: 5.6, status: "blocked", pane: "claude" },
      { at: 5.6, log: "@7 blocked · needs input" },
      { at: 5.7, signal: "needs you", from: "server", to: VIEWS },
      { at: 6.2, notice: "claude needs you · api/@7" },
    ],
  },
  {
    id: "away",
    label: "Walk away",
    title: "Close the laptop. Keep the work.",
    text: "Views come and go; the terminals keep running on the server. Answer the agent from your phone.",
    href: "/concepts",
    seconds: 8,
    steps: [
      { at: 0.6, signal: "detach", from: "terminal", to: "server" },
      { at: 0.9, view: "detached", client: "terminal" },
      { at: 0.9, line: "[detached (from session api)]", pane: "shell", tone: "dim" },
      { at: 1.5, signal: "detach", from: "desktop", to: "server" },
      { at: 1.8, view: "detached", client: "desktop" },
      { at: 2.8, tap: "y" },
      { at: 3.0, signal: "y", from: "phone", to: "server" },
      { at: 3.5, line: "y", pane: "claude", tone: "cmd" },
      { at: 3.5, status: "working", pane: "claude" },
      { at: 3.5, notice: null },
      { at: 3.5, log: "@7 input from phone" },
      { at: 3.6, line: "Applying 0042_users to staging…", pane: "claude", tone: "dim" },
      { at: 5.2, line: "✓ Staging migrated.", pane: "claude", tone: "ok" },
      { at: 5.2, status: "done", pane: "claude" },
      { at: 5.3, signal: "output", from: "server", to: "phone" },
    ],
  },
  {
    id: "remote",
    label: "Remote",
    title: "Bring your other machines.",
    text: "Pair a host over SSH once; after that the terminal attaches over QUIC. Its work keeps running where it started.",
    href: "/remote-access",
    seconds: 8.5,
    steps: [
      { at: 0, log: "" },
      { at: 0.3, type: "phux --remote me@devbox", pane: "shell" },
      { at: 1.6, remote: true },
      { at: 1.7, signal: "SSH pair", from: "terminal", to: "devbox" },
      { at: 2.7, signal: "QUIC", from: "terminal", to: "devbox" },
      { at: 3.2, view: "attached", client: "terminal" },
      { at: 3.2, focus: "train" },
      { at: 4.0, line: "epoch 12/50  loss 0.231", pane: "train", tone: "dim" },
      { at: 4.1, signal: "output", from: "devbox", to: "terminal" },
      { at: 5.4, line: "epoch 13/50  loss 0.219", pane: "train", tone: "dim" },
      { at: 5.5, signal: "output", from: "devbox", to: "terminal" },
      { at: 6.8, line: "epoch 14/50  loss 0.207", pane: "train", tone: "dim" },
      { at: 6.9, signal: "output", from: "devbox", to: "terminal" },
    ],
  },
];

function clone(frame: Frame): Frame {
  const panes = {} as Record<PaneId, Pane>;
  for (const id of Object.keys(frame.panes) as PaneId[]) {
    const pane = frame.panes[id];
    panes[id] = { lines: pane.lines.slice(), status: pane.status };
  }
  return { ...frame, panes, views: { ...frame.views }, signals: [], tap: null };
}

function play(start: Frame, steps: readonly Step[], time: number): Frame {
  const frame = clone(start);
  for (const step of steps) {
    if (step.at > time) break;
    const age = time - step.at;
    if ("line" in step) {
      frame.panes[step.pane].lines.push({ text: step.line, tone: step.tone });
    } else if ("type" in step) {
      const typed = Math.min(step.type.length, Math.floor(age * TYPE_CPS));
      frame.panes[step.pane].lines.push({
        text: (step.prompt ?? "$ ") + step.type.slice(0, typed),
        tone: step.agent ? "agent" : "cmd",
        typing: typed < step.type.length,
      });
    } else if ("status" in step) {
      frame.panes[step.pane].status = step.status;
    } else if ("view" in step) {
      frame.views[step.client] = step.view;
    } else if ("signal" in step) {
      if (age < SIGNAL_SECONDS) {
        const targets: readonly NodeId[] = typeof step.to === "string" ? [step.to] : step.to;
        for (const to of targets)
          frame.signals.push({ from: step.from, to, label: step.signal, progress: age / SIGNAL_SECONDS });
      }
    } else if ("log" in step) {
      frame.log = step.log;
    } else if ("notice" in step) {
      frame.notice = step.notice;
    } else if ("tap" in step) {
      if (age < TAP_SECONDS) frame.tap = { key: step.tap, progress: age / TAP_SECONDS };
    } else if ("focus" in step) {
      frame.focus = step.focus;
    } else {
      frame.remote = step.remote;
    }
  }
  return frame;
}

const STARTS: readonly Frame[] = CHAPTERS.reduce<Frame[]>(
  (starts, chapter, index) => {
    if (index + 1 < CHAPTERS.length) starts.push(play(starts[index]!, chapter.steps, Infinity));
    return starts;
  },
  [BASE],
);

/** The frame at `time` seconds into a chapter; `Infinity` is its final state. */
export function frameAt(chapter: number, time: number): Frame {
  return play(STARTS[chapter]!, CHAPTERS[chapter]!.steps, time);
}
