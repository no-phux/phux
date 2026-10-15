import { useEffect, useRef, useState, type CSSProperties, type ReactNode } from "react";
import { docsHref } from "../../lib/site";
import {
  CHAPTERS,
  advanceTour,
  frameAt,
  type Playback,
  type ClientId,
  type Frame,
  type NodeId,
  type Pane,
  type Status,
  type View,
} from "./script";
import "./Tour.css";

type Point = readonly [number, number];
type Curve = readonly [Point, Point, Point, Point];
type Box = readonly [number, number, number, number];
type EdgeId = ClientId | "devbox";
type Layout = {
  width: number;
  height: number;
  nodes: Record<NodeId, Box | null>;
  edges: Record<EdgeId, Curve | null>;
  /** Transport label positions where the curve midpoint would be hidden. */
  labels?: Partial<Record<EdgeId, Point>>;
};

// Coordinates are stage units; CSS maps one unit to `--u` pixels so the whole
// stage scales with its container. Each curve runs from the client to its host.
const WIDE: Layout = {
  width: 880,
  height: 480,
  nodes: {
    terminal: [24, 24, 340, 196],
    desktop: [24, 244, 404, 212],
    server: [492, 184, 176, 148],
    devbox: [492, 24, 176, 96],
    phone: [712, 92, 144, 296],
  },
  edges: {
    terminal: [[364, 150], [430, 150], [430, 236], [492, 236]],
    desktop: [[428, 350], [464, 350], [464, 280], [492, 280]],
    phone: [[712, 240], [690, 240], [690, 258], [668, 258]],
    devbox: [[364, 72], [428, 72], [428, 72], [492, 72]],
  },
};

const NARROW: Layout = {
  width: 360,
  height: 540,
  nodes: {
    terminal: [12, 12, 336, 176],
    desktop: null,
    server: [12, 224, 172, 128],
    devbox: [12, 376, 172, 84],
    phone: [196, 224, 152, 300],
  },
  edges: {
    terminal: [[98, 188], [98, 204], [98, 208], [98, 224]],
    desktop: null,
    phone: [[196, 312], [190, 312], [190, 288], [184, 288]],
    devbox: [[16, 188], [2, 260], [2, 418], [12, 418]],
  },
  labels: { devbox: [44, 210], terminal: [130, 210] },
};

const TRANSPORT: Record<EdgeId, string> = {
  terminal: "local socket",
  desktop: "local socket",
  phone: "QUIC",
  devbox: "QUIC",
};

const BADGE: Record<Status, { glyph: string; label: string }> = {
  blocked: { glyph: "●", label: "needs input" },
  done: { glyph: "◆", label: "done" },
  working: { glyph: "◐", label: "working" },
  idle: { glyph: "○", label: "idle" },
};

function cubic([p0, c1, c2, p3]: Curve, t: number): Point {
  const u = 1 - t;
  const a = u * u * u, b = 3 * u * u * t, c = 3 * u * t * t, d = t * t * t;
  return [
    a * p0[0] + b * c1[0] + c * c2[0] + d * p3[0],
    a * p0[1] + b * c1[1] + c * c2[1] + d * p3[1],
  ];
}

function edgeView(frame: Frame, edge: EdgeId): View {
  if (edge === "devbox") return frame.remote ? "attached" : "off";
  if (edge === "terminal" && frame.remote) return "detached";
  return frame.views[edge];
}

// Lines render beneath the nodes and signals above them, so each layout draws twice.
function Edges({ layout, frame, className, signals = false }: {
  layout: Layout; frame: Frame; className: string; signals?: boolean;
}) {
  const edges = (Object.keys(TRANSPORT) as EdgeId[]).filter((edge) => layout.edges[edge]);
  return (
    <svg className={`tour-edges ${className}`} viewBox={`0 0 ${layout.width} ${layout.height}`}>
      {!signals && edges.map((edge) => {
        const curve = layout.edges[edge]!;
        const [p0, c1, c2, p3] = curve;
        const [mx, my] = layout.labels?.[edge] ?? cubic(curve, 0.5);
        return (
          <g key={edge} className="tour-edge" data-edge={edge} data-view={edgeView(frame, edge)}>
            <path d={`M${p0} C${c1} ${c2} ${p3}`} />
            <text x={mx} y={my - 6}>{TRANSPORT[edge]}</text>
          </g>
        );
      })}
      {signals && frame.signals.map((signal, index) => {
        const edge: EdgeId =
          signal.from === "devbox" || signal.to === "devbox"
            ? "devbox"
            : ((signal.from === "server" ? signal.to : signal.from) as ClientId);
        const curve = layout.edges[edge];
        if (!curve) return null;
        // Minimum-jerk travel; curves run client → host.
        const t = signal.progress;
        const eased = t * t * t * (t * (t * 6 - 15) + 10);
        const forward = signal.from === (edge === "devbox" ? "terminal" : edge);
        const [x, y] = cubic(curve, forward ? eased : 1 - eased);
        const fade = Math.min(1, (1 - t) * 4);
        return (
          <g key={index} className="tour-signal" style={{ opacity: fade }}>
            <circle cx={x} cy={y} r={7} className="tour-signal-halo" />
            <circle cx={x} cy={y} r={3.5} />
            <text x={x} y={y - 11}>{signal.label}</text>
          </g>
        );
      })}
    </svg>
  );
}

function Badge({ status }: { status: Status }) {
  return <span className={`tour-badge is-${status}`} title={BADGE[status].label}>{BADGE[status].glyph}</span>;
}

function Lines({ pane, prompt = false }: { pane: Pane; prompt?: boolean }) {
  const lines = pane.lines.slice(-7);
  const typing = lines.at(-1)?.typing;
  return (
    <div className="tour-lines">
      {lines.map((line, index) => (
        <span key={index} className={line.tone && `is-${line.tone}`}>
          {line.text}
          {line.typing && <b className="tour-caret" />}
        </span>
      ))}
      {prompt && !typing && <span className="is-cmd">$ <b className="tour-caret" /></span>}
    </div>
  );
}

function boxStyle(id: NodeId): CSSProperties {
  const wide = WIDE.nodes[id]!;
  const narrow = NARROW.nodes[id] ?? wide;
  const style: Record<string, number> = {};
  ["x", "y", "w", "h"].forEach((key, index) => {
    style[`--${key}`] = wide[index]!;
    style[`--n${key}`] = narrow[index]!;
  });
  return style as CSSProperties;
}

function Node({ id, className, view, children }: {
  id: NodeId; className: string; view?: View; children: ReactNode;
}) {
  return (
    <div
      className={`tour-node ${className}${NARROW.nodes[id] ? "" : " is-wide-only"}`}
      style={boxStyle(id)}
      data-view={view}
    >
      {children}
    </div>
  );
}

function Chrome({ title, extra }: { title: string; extra?: ReactNode }) {
  return (
    <div className="tour-chrome">
      <span className="tour-lights"><i /><i /><i /></span>
      <span className="tour-chrome-title">{title}</span>
      {extra}
    </div>
  );
}

function PaneTabs({ items, active }: { items: readonly [string, string, Status][]; active?: string }) {
  return (
    <div className="tour-tabs">
      {items.map(([id, name, status]) => (
        <span key={id} className={id === active ? "is-active" : undefined}>
          {id} {name} <Badge status={status} />
        </span>
      ))}
    </div>
  );
}

function Terminal({ frame }: { frame: Frame }) {
  const attached = frame.views.terminal === "attached";
  const title = !attached ? "zsh — studio" : frame.remote ? "phux — me@devbox" : "phux attach api";
  const pane = !attached ? frame.panes.shell : frame.panes[frame.remote ? "train" : frame.focus];
  const tabs: [string, string, Status][] = frame.remote
    ? [["@2", "train", frame.panes.train.status]]
    : [["@3", "build", frame.panes.build.status], ["@7", "claude", frame.panes.claude.status]];
  return (
    <Node id="terminal" className="tour-window tour-terminal" view={frame.views.terminal}>
      <Chrome title={title} />
      <div className="tour-body"><Lines pane={pane} prompt={!attached} /></div>
      {attached && (
        <div className="tour-tmux">
          <span className="tour-session">{frame.remote ? "devbox" : "api"}</span>
          <PaneTabs items={tabs} active={frame.remote ? "@2" : frame.focus === "build" ? "@3" : "@7"} />
        </div>
      )}
    </Node>
  );
}

function Desktop({ frame }: { frame: Frame }) {
  const view = frame.views.desktop;
  return (
    <Node id="desktop" className="tour-window tour-desktop" view={view}>
      <Chrome
        title="Phux.app — api"
        extra={
          <span className={`tour-bell${frame.notice ? " is-ringing" : ""}`} aria-hidden="true">
            <svg viewBox="0 0 16 16"><path d="M8 2a4 4 0 0 0-4 4v3L2.5 11.5h11L12 9V6a4 4 0 0 0-4-4Zm-1.6 11a1.6 1.6 0 0 0 3.2 0" /></svg>
            {frame.notice && <em>1</em>}
          </span>
        }
      />
      <div className="tour-split">
        {(["build", "claude"] as const).map((id) => (
          <div key={id} className="tour-split-pane">
            <PaneTabs items={[[id === "build" ? "@3" : "@7", id, frame.panes[id].status]]} />
            <div className="tour-body"><Lines pane={frame.panes[id]} /></div>
          </div>
        ))}
      </div>
      {frame.notice && view === "attached" && <div className="tour-toast"><Badge status="blocked" /> {frame.notice}</div>}
      {view !== "attached" && (
        <div className="tour-veil">
          {view === "off" ? "Opening Phux.app…" : <>Window closed<small>the terminals keep running</small></>}
        </div>
      )}
    </Node>
  );
}

function Phone({ frame }: { frame: Frame }) {
  const view = frame.views.phone;
  const id = frame.focus === "build" ? "build" : "claude";
  const pane = frame.panes[id];
  return (
    <Node id="phone" className="tour-phone" view={view}>
      <div className="tour-phone-screen">
        <div className="tour-phone-status"><span>9:41</span><i /><span>QUIC</span></div>
        {view === "attached" ? (
          <>
            <div className="tour-phone-head">
              <span>api</span>
              <span>{id === "build" ? "@3" : "@7"} {id} <Badge status={pane.status} /></span>
            </div>
            {pane.status === "blocked" && <div className="tour-phone-alert">Needs you</div>}
            <div className="tour-body"><Lines pane={pane} /></div>
            <div className="tour-phone-keys">
              {["esc", "tab", "ctrl", "y", "n"].map((key) => (
                <span
                  key={key}
                  className={frame.tap?.key === key ? "is-tapped" : undefined}
                  style={frame.tap?.key === key ? ({ "--tap": frame.tap.progress } as CSSProperties) : undefined}
                >
                  {key}
                </span>
              ))}
            </div>
          </>
        ) : (
          <div className="tour-phone-idle"><b>phux</b><span>studio · paired</span></div>
        )}
      </div>
    </Node>
  );
}

function Host({ frame, id }: { frame: Frame; id: "server" | "devbox" }) {
  const local = id === "server";
  const rows: [string, string, Status][] = local
    ? [["@3", "build", frame.panes.build.status], ["@7", "claude", frame.panes.claude.status]]
    : [["@2", "train", frame.panes.train.status]];
  const views = local
    ? (["terminal", "desktop", "phone"] as const).filter((client) => edgeView(frame, client) === "attached").length
    : 1;
  return (
    <Node id={id} className={`tour-host tour-${id}`} view={local || frame.remote ? "attached" : "off"}>
      <div className="tour-host-head">
        <span className="tour-host-dot" />
        phux server
        <span className="tour-host-name">{local ? "studio" : "devbox"}</span>
      </div>
      <ul>
        {rows.map(([ref, name, status]) => (
          <li key={ref}><span>{ref}</span> {name} <Badge status={status} /></li>
        ))}
      </ul>
      {local && <div className="tour-host-log">{frame.log || `${views} ${views === 1 ? "view" : "views"} attached`}</div>}
    </Node>
  );
}

function useReducedMotion() {
  // Start still during hydration, including before the preference is known.
  const [reduced, setReduced] = useState(true);
  useEffect(() => {
    const preference = window.matchMedia("(prefers-reduced-motion: reduce)");
    const update = () => setReduced(preference.matches);
    update();
    preference.addEventListener("change", update);
    return () => preference.removeEventListener("change", update);
  }, []);
  return reduced;
}

// Quiet context, not extra actors in the story. These hosts never claim live status.
function DistantHosts() {
  return <div className="tour-distance" aria-hidden="true">
    <svg viewBox="0 0 880 480" preserveAspectRatio="none">
      <path d="M580 240 C750 240 580 42 770 42 M580 240 C660 240 640 440 770 440 M580 240 C480 240 560 410 470 410" />
    </svg>
    {["relay / iad", "runner / fra", "lab / sea"].map((host, index) => (
      <div className={`tour-distant-host is-${index}`} key={host}>
        <span>{host}</span><i /><i /><i />
      </div>
    ))}
  </div>;
}

export default function Tour({ prepare, suspended = false }: { prepare?: () => void; suspended?: boolean }) {
  const root = useRef<HTMLDivElement>(null);
  const reduced = useReducedMotion();
  const [visible, setVisible] = useState(false);
  const [playing, setPlaying] = useState(true);
  // Server render and reduced motion show each chapter's settled state.
  const [position, setPosition] = useState<Playback>({ chapter: 0, time: Infinity, pace: "quick" });
  const clock = useRef(position);
  const chapter = CHAPTERS[position.chapter]!;

  function show(next: Playback) {
    clock.current = next;
    setPosition(next);
  }

  useEffect(() => {
    show({ ...clock.current, time: reduced ? Infinity : 0 });
  }, [reduced]);

  useEffect(() => {
    const observer = new IntersectionObserver(([entry]) => setVisible(Boolean(entry?.isIntersecting)));
    if (root.current) observer.observe(root.current);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    if (!playing || reduced || !visible || suspended) return;
    let frame = 0;
    let previous = performance.now();
    const tick = (now: number) => {
      // Hidden-tab time is not story time; resume where the viewer left off.
      const delta = Math.min((now - previous) / 1000, 0.1);
      previous = now;
      if (!document.hidden) show(advanceTour(clock.current, delta));
      frame = requestAnimationFrame(tick);
    };
    frame = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(frame);
  }, [playing, reduced, visible, suspended]);

  const frame = frameAt(position.chapter, position.time);
  const progress = Math.min(1, position.time / chapter.seconds);

  return (
    <div ref={root} className="tour" data-pace={position.pace} data-chapter={chapter.id} data-playing={playing && !reduced && visible && !suspended}>
      <div className="tour-frame terminal-surface">
        <header className="tour-heading">
          <div>
            <p className="tour-eyebrow">The phux workflow <span>0{position.chapter + 1} / 0{CHAPTERS.length}</span></p>
            <h3 key={chapter.id}>{chapter.title}</h3>
          </div>
          <span className="tour-illustration">Illustrated tour</span>
        </header>
        <div className="tour-canvas" onPointerEnter={prepare}>
          <div className="tour-stage" aria-hidden="true">
            <DistantHosts />
            <Edges layout={WIDE} frame={frame} className="is-wide" />
            <Edges layout={NARROW} frame={frame} className="is-narrow" />
            <Host frame={frame} id="server" />
            <Host frame={frame} id="devbox" />
            <Terminal frame={frame} />
            <Desktop frame={frame} />
            <Phone frame={frame} />
            <Edges layout={WIDE} frame={frame} className="is-wide" signals />
            <Edges layout={NARROW} frame={frame} className="is-narrow" signals />
          </div>
          <a className="tour-entry" href="/embed" data-demo-launch onFocus={prepare} aria-label="Open terminal demo">
            <span className="tour-invitation">
              <span className="tour-invitation-copy">
                <span className="tour-invitation-glyph" aria-hidden="true">↳</span>
                <b>Take the controls.</b>
                <span className="tour-invitation-cta">Try the real demo<span aria-hidden="true"> ↗</span></span>
              </span>
            </span>
          </a>
        </div>
        <div className="tour-controls">
          <div className="tour-chapters" role="group" aria-label="Tour chapters">
            {CHAPTERS.map((item, index) => (
              <button
                key={item.id}
                type="button"
                aria-pressed={index === position.chapter}
                style={{ "--progress": index === position.chapter ? progress : 0 } as CSSProperties}
                onClick={() => {
                  show({ chapter: index, time: reduced ? Infinity : 0, pace: "walkthrough" });
                  setPlaying(true);
                }}
              >
                <span>0{index + 1}</span> {item.label}
              </button>
            ))}
          </div>
          {!reduced && (
            <button
              type="button"
              className="tour-play"
              aria-label={playing ? "Pause tour" : "Play tour"}
              onClick={() => setPlaying((active) => !active)}
            >
              {playing
                ? <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M5 3h2v10H5zM9 3h2v10H9z" /></svg>
                : <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M5 3l8 5-8 5z" /></svg>}
            </button>
          )}
        </div>
      </div>
      <p className="tour-caption">
        {chapter.text}{" "}
        <a href={docsHref(chapter.href)}>Learn more<span className="tour-sr"> about {chapter.label.toLowerCase()}</span></a>
      </p>
      <p className="tour-note">
        {!reduced && <span className="tour-pace">{position.pace === "quick" ? "Quick pass · walkthrough next. " : "Walkthrough · select a chapter to replay. "}</span>}
        Scripted illustration, not a live session. The iPhone app is in TestFlight beta.
      </p>
    </div>
  );
}
