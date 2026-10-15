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

// Connection motion stays crisp even in the explanatory lap. Reading time
// belongs after a result, not in pretend network latency or slow typing.
export const SIGNAL_SECONDS = 0.18;
const TAP_SECONDS = 0.22;
const TYPE_CPS = 110;
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
    seconds: 4.2,
    steps: [
      { at: 0.08, type: "phux attach api", pane: "shell" },
      { at: 0.24, signal: "attach", from: "terminal", to: "server" },
      { at: 0.44, view: "attached", client: "terminal" },
      { at: 0.48, type: "cargo build --release", pane: "build" },
      { at: 0.70, signal: "keys", from: "terminal", to: "server" },
      { at: 0.90, status: "working", pane: "build" },
      { at: 0.92, line: "   Compiling api v0.4.2", pane: "build", tone: "dim" },
      { at: 0.96, signal: "attach", from: "desktop", to: "server" },
      { at: 1.16, view: "attached", client: "desktop" },
      { at: 1.20, signal: "attach", from: "phone", to: "server" },
      { at: 1.40, view: "attached", client: "phone" },
      { at: 1.52, line: "   Compiling tokio v1.47.1", pane: "build", tone: "dim" },
      { at: 1.54, signal: "output", from: "server", to: VIEWS },
      { at: 2.06, line: "    Finished `release` in 41s", pane: "build", tone: "ok" },
      { at: 2.06, status: "done", pane: "build" },
      { at: 2.08, signal: "output", from: "server", to: VIEWS },
    ],
  },
  {
    id: "agents",
    label: "Agents",
    title: "Agents work in the open.",
    text: "Claude runs in a pane and drives other terminals through phux. Its state reaches every screen, so you see when it needs you.",
    href: "/consumers/getting-started",
    seconds: 4.8,
    steps: [
      { at: 0, focus: "claude" },
      { at: 0.08, type: "fix the flaky migration test", pane: "claude", prompt: "> " },
      { at: 0.36, signal: "keys", from: "terminal", to: "server" },
      { at: 0.56, status: "working", pane: "claude" },
      { at: 0.58, line: "Reading migrations/0042_users.sql", pane: "claude", tone: "dim" },
      { at: 0.90, log: "@7 → phux_run @3 cargo test" },
      { at: 0.92, type: "cargo test", pane: "build", agent: true },
      { at: 1.08, status: "working", pane: "build" },
      { at: 1.10, line: "running 128 tests", pane: "build", tone: "dim" },
      { at: 1.12, signal: "output", from: "server", to: VIEWS },
      { at: 1.48, line: "test result: ok. 128 passed", pane: "build", tone: "ok" },
      { at: 1.48, status: "done", pane: "build" },
      { at: 1.90, line: "Tests pass. Apply 0042 to staging? (y/n)", pane: "claude", tone: "ask" },
      { at: 1.90, status: "blocked", pane: "claude" },
      { at: 1.90, log: "@7 blocked · needs input" },
      { at: 1.92, signal: "needs you", from: "server", to: VIEWS },
      { at: 2.12, notice: "claude needs you · api/@7" },
    ],
  },
  {
    id: "away",
    label: "Walk away",
    title: "Close the laptop. Keep the work.",
    text: "Views come and go; the terminals keep running on the server. Answer the agent from your phone.",
    href: "/concepts",
    seconds: 4.5,
    steps: [
      { at: 0.08, signal: "detach", from: "terminal", to: "server" },
      { at: 0.28, view: "detached", client: "terminal" },
      { at: 0.28, line: "[detached (from session api)]", pane: "shell", tone: "dim" },
      { at: 0.34, signal: "detach", from: "desktop", to: "server" },
      { at: 0.54, view: "detached", client: "desktop" },
      { at: 1.30, tap: "y" },
      { at: 1.34, signal: "y", from: "phone", to: "server" },
      { at: 1.54, line: "y", pane: "claude", tone: "cmd" },
      { at: 1.54, status: "working", pane: "claude" },
      { at: 1.54, notice: null },
      { at: 1.54, log: "@7 input from phone" },
      { at: 1.56, line: "Applying 0042_users to staging…", pane: "claude", tone: "dim" },
      { at: 2.08, line: "✓ Staging migrated.", pane: "claude", tone: "ok" },
      { at: 2.08, status: "done", pane: "claude" },
      { at: 2.10, signal: "output", from: "server", to: "phone" },
    ],
  },
  {
    id: "remote",
    label: "Remote",
    title: "Bring your other machines.",
    text: "Pair a host over SSH once; after that the terminal attaches over QUIC. Its work keeps running where it started.",
    href: "/remote-access",
    seconds: 4.2,
    steps: [
      { at: 0, log: "" },
      { at: 0.08, type: "phux --remote me@devbox", pane: "shell" },
      { at: 0.32, remote: true },
      { at: 0.34, signal: "SSH pair", from: "terminal", to: "devbox" },
      { at: 0.60, signal: "QUIC", from: "terminal", to: "devbox" },
      { at: 0.80, view: "attached", client: "terminal" },
      { at: 0.80, focus: "train" },
      { at: 1.04, line: "epoch 12/50  loss 0.231", pane: "train", tone: "dim" },
      { at: 1.06, signal: "output", from: "devbox", to: "terminal" },
      { at: 1.40, line: "epoch 13/50  loss 0.219", pane: "train", tone: "dim" },
      { at: 1.42, signal: "output", from: "devbox", to: "terminal" },
      { at: 1.76, line: "epoch 14/50  loss 0.207", pane: "train", tone: "dim" },
      { at: 1.78, signal: "output", from: "devbox", to: "terminal" },
    ],
  },
];

export type Playback = {
  chapter: number;
  time: number;
  pace: "quick" | "walkthrough";
};

export const QUICK_RATE = 3;

/** Advance one visible animation frame (delta is capped at 0.1s by the player).
 * The opening lap is quick; every subsequent lap uses the readable timeline.
 * Carry wall-clock overshoot across the pace change rather than losing time.
 */
export function advanceTour(position: Playback, delta: number): Playback {
  const rate = position.pace === "quick" ? QUICK_RATE : 1;
  const remaining = (CHAPTERS[position.chapter]!.seconds - position.time) / rate;
  if (delta < remaining) return { ...position, time: position.time + delta * rate };
  const chapter = (position.chapter + 1) % CHAPTERS.length;
  const pace = chapter === 0 ? "walkthrough" : position.pace;
  return { chapter, pace, time: (delta - remaining) * (pace === "quick" ? QUICK_RATE : 1) };
}

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
