export type NodeId = "client" | "server" | "terminal" | "agent" | "remote";
export type Beat = {
  title: string;
  text: string;
  detail: string;
  from?: NodeId;
  to?: NodeId;
  signal?: string;
  detached?: boolean;
};
export type Scenario = {
  id: string;
  label: string;
  title: string;
  summary: string;
  href: string;
  beats: readonly Beat[];
};

export const CLIENTS = ["Cockpit", "Terminal", "Browser", "iPhone"] as const;
export const BEAT_SECONDS = 2.5;
export const SCENARIOS: readonly Scenario[] = [
  {
    id: "continuity",
    label: "Leave. Come back.",
    title: "Close the view. Not the work.",
    summary:
      "Your terminal lives on the machine, not inside the window you happen to be using.",
    href: "/concepts",
    beats: [
      {
        title: "Open a view",
        text: "A client attaches to work owned by the background server.",
        detail:
          "Attach opens a terminal subscription; it does not copy the running process.",
        from: "client",
        to: "server",
        signal: "attach",
      },
      {
        title: "The program runs",
        text: "Shells, editors, builds: real programs in real terminals.",
        detail: "The server owns the PTY child and libghostty terminal state.",
        from: "server",
        to: "terminal",
        signal: "input",
      },
      {
        title: "Walk away",
        text: "Disconnect the client. The server and program keep running.",
        detail:
          "Detach ends a view, not the PTY. This is not crash recovery: server death ends live work.",
        detached: true,
      },
      {
        title: "Pick it up elsewhere",
        text: "Attach from another client and return to the same running terminal.",
        detail:
          "Bootstrap restores rendering state before live output; no process migration.",
        from: "server",
        to: "client",
        signal: "current state + output",
      },
    ],
  },
  {
    id: "agents",
    label: "Human + agents",
    title: "An agent works. You stay in the loop.",
    summary:
      "Automation and people reach the same terminal runtime through public interfaces.",
    href: "/consumers/agents",
    beats: [
      {
        title: "Give it a task",
        text: "An agent uses CLI, MCP, or protocol tools to control a terminal.",
        detail:
          "Agent tools use the public control plane, not a privileged UI backdoor.",
        from: "agent",
        to: "server",
        signal: "tool request",
      },
      {
        title: "Work happens on the host",
        text: "The server delivers input to the terminal where the work runs.",
        detail:
          "Terminal input is structured key, mouse, focus, or paste events.",
        from: "server",
        to: "terminal",
        signal: "terminal input",
      },
      {
        title: "See what happened",
        text: "Output and agent state let you observe progress without taking over.",
        detail:
          "Harness integrations emit lifecycle state; pane detection is the fallback. Viewer attach is read-only.",
        from: "server",
        to: "client",
        signal: "output + state",
      },
      {
        title: "Coordinate the handoff",
        text: "Return to the terminal when the agent needs your attention.",
        detail:
          "Multiple writers can interleave input. Coordinate before typing into an agent-driven terminal.",
        from: "client",
        to: "server",
        signal: "human input",
      },
    ],
  },
  {
    id: "federation",
    label: "Across machines",
    title: "One view. More than one machine.",
    summary:
      "A hub exposes remote resources. Each program stays on the host that owns it.",
    href: "/remote-access",
    beats: [
      {
        title: "Reach the hub",
        text: "Your client connects to a phux server with configured remote hosts.",
        detail:
          "Remote connections carry the same phux protocol over the chosen transport.",
        from: "client",
        to: "server",
        signal: "connect",
      },
      {
        title: "Discover remote work",
        text: "The hub brings another machine’s resource inventory into view.",
        detail:
          "Satellite resource IDs include a host identity. Remote session and window models are not merged.",
        from: "remote",
        to: "server",
        signal: "host / resource",
      },
      {
        title: "Route to the owner",
        text: "Select remote work; supported operations route to its owning host.",
        detail:
          "The hub retags and routes resources; it does not move a PTY or replicate a running process.",
        from: "server",
        to: "remote",
        signal: "routed operation",
      },
      {
        title: "See it from your client",
        text: "Remote terminal output returns through the hub to your screen.",
        detail:
          "This is a conceptual routing diagram, not a promise that every operation supports satellite routes.",
        from: "server",
        to: "client",
        signal: "remote output",
      },
    ],
  },
  {
    id: "wire",
    label: "Under the hood",
    title: "Input goes in. Terminal bytes come back.",
    summary:
      "A terminal control plane, not a screen-video stream. The engine lives at both ends.",
    href: "/wire/proto",
    beats: [
      {
        title: "Describe the input",
        text: "A client sends structured input rather than a screen-video stream.",
        detail:
          "Key, mouse, focus, and paste events are encoded as protocol atoms.",
        from: "client",
        to: "server",
        signal: "structured input",
      },
      {
        title: "Deliver to the program",
        text: "The server translates input for the PTY child.",
        detail:
          "The server-side terminal engine knows the program’s terminal modes.",
        from: "server",
        to: "terminal",
        signal: "PTY input",
      },
      {
        title: "Read terminal output",
        text: "The program writes VT bytes. The server maintains terminal state.",
        detail:
          "libghostty tracks screen, scrollback, and modes; human output normally follows the raw PTY broadcast path.",
        from: "terminal",
        to: "server",
        signal: "VT bytes",
      },
      {
        title: "Render at the edge",
        text: "Clients render terminal output locally and send the next input back.",
        detail:
          "Bootstrap precedes live output. Client rendering is a replica, not another running shell.",
        from: "server",
        to: "client",
        signal: "VT bytes",
      },
    ],
  },
];

export function duration(scenario: Scenario): number {
  return scenario.beats.length * BEAT_SECONDS;
}

export function beatAt(scenario: Scenario, time: number): number {
  return Math.min(
    scenario.beats.length - 1,
    Math.floor(Math.max(0, time) / BEAT_SECONDS),
  );
}

// Minimum-jerk travel: continuous velocity and acceleration at both ports.
export function smootherstep(value: number): number {
  const t = Math.min(1, Math.max(0, value));
  return t * t * t * (t * (t * 6 - 15) + 10);
}

export function signalProgress(time: number): number {
  const age = Math.min(BEAT_SECONDS, Math.max(0, time));
  return smootherstep((age - 0.35) / 1.3);
}
