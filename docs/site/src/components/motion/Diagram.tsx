import {
  BEAT_SECONDS,
  beatAt,
  signalProgress,
  type Beat,
  type NodeId,
  type Scenario,
} from "./scenarios";

type Point = [number, number];
type Layout = Record<NodeId, Point>;
const DESKTOP: Layout = {
  client: [125, 160],
  server: [440, 160],
  terminal: [740, 160],
  agent: [125, 350],
  remote: [740, 350],
};
const MOBILE: Layout = {
  client: [160, 90],
  server: [160, 225],
  terminal: [160, 360],
  agent: [160, 505],
  remote: [160, 505],
};

export function port(center: Point, toward: Point): Point {
  const dx = toward[0] - center[0];
  const dy = toward[1] - center[1];
  const ratio = Math.min(99 / Math.abs(dx), 38 / Math.abs(dy));
  return [center[0] + dx * ratio, center[1] + dy * ratio];
}

export function curvePoint(
  start: Point,
  control: Point,
  end: Point,
  progress: number,
): Point {
  const rest = 1 - progress;
  return [
    rest * rest * start[0] +
      2 * rest * progress * control[0] +
      progress * progress * end[0],
    rest * rest * start[1] +
      2 * rest * progress * control[1] +
      progress * progress * end[1],
  ];
}

function link(layout: Layout, from: NodeId, to: NodeId, compact: boolean) {
  const a = layout[from];
  const b = layout[to];
  const control: Point = [(a[0] + b[0]) / 2, (a[1] + b[1]) / 2];
  if (compact && Math.abs(a[1] - b[1]) > 200) control[0] = -100;
  const start = port(a, control);
  const end = port(b, control);
  return { start, control, end, path: `M${start} Q${control} ${end}` };
}

function Signal({
  route,
  age,
  label,
  reduced,
}: {
  route: ReturnType<typeof link>;
  age: number;
  label?: string;
  reduced: boolean;
}) {
  const progress = signalProgress(age);
  const [x, y] = curvePoint(route.start, route.control, route.end, progress);
  const [trailX, trailY] = curvePoint(
    route.start,
    route.control,
    route.end,
    Math.max(0, progress - 0.12),
  );
  const arrival = Math.min(1, Math.max(0, (age - 1.65) / 0.85));
  if (reduced) return <path d={route.path} className="active-wire" />;
  return (
    <g>
      <path d={route.path} className="active-wire" />
      <g className="signal-packet">
        <line x1={trailX} y1={trailY} x2={x} y2={y} className="packet-trail" />
        <circle cx={x} cy={y} r="13" className="packet-halo" />
        <circle cx={x} cy={y} r="5" />
        <text x={x} y={y - 20} textAnchor="middle" className="packet-label">
          {label}
        </text>
      </g>
      {progress === 1 && (
        <circle
          cx={route.end[0]}
          cy={route.end[1]}
          r={6 + 24 * arrival}
          opacity={1 - arrival}
          className="arrival-ring"
        />
      )}
    </g>
  );
}

function StageNode({
  position,
  name,
  sub,
  landed,
  detached,
}: {
  position: Point;
  name: string;
  sub: string;
  landed: boolean;
  detached: boolean;
}) {
  return (
    <g
      transform={`translate(${position})`}
      className={`stage-node ${landed ? "landed" : ""} ${detached ? "detached" : ""}`}
    >
      <rect x="-99" y="-38" width="198" height="76" rx="8" />
      <circle cx="-80" cy="-19" r="3" className="node-port" />
      <text textAnchor="middle" y="0" className="node-name">
        {name}
      </text>
      <text textAnchor="middle" y="23" className="node-sub">
        {detached ? "view disconnected" : sub}
      </text>
    </g>
  );
}

function actorLabels(client: string, technical: boolean, scenario: Scenario) {
  return [
    {
      id: "client" as const,
      name: client,
      sub: technical ? "render + input" : "your view",
    },
    {
      id: "server" as const,
      name: scenario.id === "federation" ? "phux hub" : "phux server",
      sub: "background runtime",
    },
    {
      id: "terminal" as const,
      name: technical ? "PTY + libghostty" : "Your terminal",
      sub: "shell / editor / build",
    },
    ...EXTRA_ACTORS[scenario.id]!,
  ];
}
const EXTRA_ACTORS: Record<
  string,
  { id: NodeId; name: string; sub: string }[]
> = {
  continuity: [],
  wire: [],
  agents: [{ id: "agent", name: "Agent / bot", sub: "CLI / MCP / protocol" }],
  federation: [
    { id: "remote", name: "Remote machine", sub: "its server, its programs" },
  ],
};

function composition(compact: boolean, actorCount: number) {
  if (!compact)
    return {
      layout: DESKTOP,
      viewBox: actorCount > 3 ? "0 0 870 420" : "0 0 870 270",
      className: "motion-diagram wide",
    };
  const height = actorCount > 3 ? 580 : 435;
  return {
    layout: MOBILE,
    viewBox: `0 0 320 ${height}`,
    className: "motion-diagram compact",
  };
}

function activeRoute(layout: Layout, beat: Beat, compact: boolean) {
  if (!beat.from || !beat.to) return null;
  return link(layout, beat.from, beat.to, compact);
}

function NodeForBeat({
  actor,
  beat,
  layout,
  progress,
}: {
  actor: { id: NodeId; name: string; sub: string };
  beat: Beat;
  layout: Layout;
  progress: number;
}) {
  const landed = actor.id === beat.to && progress >= 0.98;
  const detached = actor.id === "client" && Boolean(beat.detached);
  return (
    <StageNode
      position={layout[actor.id]}
      name={actor.name}
      sub={actor.sub}
      landed={landed}
      detached={detached}
    />
  );
}

function IdleLinks({
  actors,
  layout,
  compact,
}: {
  actors: { id: NodeId }[];
  layout: Layout;
  compact: boolean;
}) {
  return (
    <g className="quiet-wires">
      {actors.slice(1).map((actor) => (
        <path
          key={actor.id}
          d={
            link(
              layout,
              "server",
              actor.id === "server" ? "client" : actor.id,
              compact,
            ).path
          }
        />
      ))}
    </g>
  );
}

function HostBoundary({ compact }: { compact: boolean }) {
  const box = compact
    ? { x: 40, y: 164, width: 240, height: 246, labelX: 62, labelY: 180 }
    : { x: 315, y: 80, width: 530, height: 155, labelX: 330, labelY: 102 };
  return (
    <g>
      <rect
        x={box.x}
        y={box.y}
        width={box.width}
        height={box.height}
        rx="12"
        className="host-boundary"
      />
      <text x={box.labelX} y={box.labelY} className="host-label">
        HOST MACHINE / WORK STAYS HERE
      </text>
    </g>
  );
}

export default function Diagram({
  scenario,
  time,
  client,
  technical,
  reduced,
  compact = false,
}: {
  scenario: Scenario;
  time: number;
  client: string;
  technical: boolean;
  reduced: boolean;
  compact?: boolean;
}) {
  const active = beatAt(scenario, time);
  const beat = scenario.beats[active]!;
  const age = time - active * BEAT_SECONDS;
  const progress = signalProgress(age);
  const actors = actorLabels(client, technical, scenario);
  const { layout, viewBox, className } = composition(compact, actors.length);
  const route = activeRoute(layout, beat, compact);
  return (
    <svg
      viewBox={viewBox}
      role="img"
      aria-label={`${beat.title}. ${beat.text}`}
      className={className}
    >
      <text x={compact ? 24 : 30} y="30" className="stage-heading">
        {technical ? "CONTROL / OUTPUT" : "FOLLOW THE WORK"}
      </text>
      <text x="24" y="46" className="signal-legend">
        {beat.signal}
      </text>
      <HostBoundary compact={compact} />
      <IdleLinks actors={actors} layout={layout} compact={compact} />
      {actors.map((actor) => (
        <NodeForBeat
          key={actor.id}
          actor={actor}
          beat={beat}
          layout={layout}
          progress={progress}
        />
      ))}
      {route && (
        <Signal route={route} age={age} label={beat.signal} reduced={reduced} />
      )}
    </svg>
  );
}
