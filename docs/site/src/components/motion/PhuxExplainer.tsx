import { useEffect, useRef, useState } from "react";
import { docsHref } from "../../lib/site";
import {
  BEAT_SECONDS,
  CLIENTS,
  SCENARIOS,
  beatAt,
  duration,
  type Scenario,
} from "./scenarios";
import Diagram from "./Diagram";
import "./PhuxExplainer.css";

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

function useStageVisibility() {
  const stage = useRef<HTMLDivElement>(null);
  const [visible, setVisible] = useState(false);
  useEffect(() => {
    const observer = new IntersectionObserver(([entry]) =>
      setVisible(Boolean(entry?.isIntersecting)),
    );
    if (stage.current) observer.observe(stage.current);
    return () => observer.disconnect();
  }, []);
  return { stage, visible };
}

function usePlayback(scenario: Scenario, reduced: boolean, visible: boolean) {
  const [time, setTime] = useState(0);
  const [playing, setPlaying] = useState(false);
  const clock = useRef(0);
  useEffect(() => {
    if (!playing || reduced || !visible) return;
    let frame = 0;
    let previous = performance.now();
    const tick = (now: number) => {
      // Hidden-tab time is not story time. Resume where the viewer left off.
      const delta = Math.min((now - previous) / 1000, 0.1);
      previous = now;
      if (!document.hidden)
        clock.current = Math.min(duration(scenario), clock.current + delta);
      setTime(clock.current);
      if (clock.current >= duration(scenario)) {
        setPlaying(false);
        return;
      }
      frame = requestAnimationFrame(tick);
    };
    frame = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(frame);
  }, [playing, scenario, reduced, visible]);
  useEffect(() => {
    if (reduced) setPlaying(false);
  }, [reduced]);
  function seek(value: number) {
    clock.current = value;
    setTime(value);
    setPlaying(false);
  }
  function toggle() {
    if (time >= duration(scenario)) seek(0);
    setPlaying((active) => !active);
  }
  return { time, playing, seek, toggle };
}

export default function PhuxExplainer() {
  const [selected, setSelected] = useState(0);
  const [technical, setTechnical] = useState(false);
  const [client, setClient] = useState<string>(CLIENTS[0]);
  const reduced = useReducedMotion();
  const { stage, visible } = useStageVisibility();
  const scenario = SCENARIOS[selected]!;
  const playback = usePlayback(scenario, reduced, visible);
  const active = beatAt(scenario, playback.time);
  const beat = scenario.beats[active]!;
  function selectScenario(index: number) {
    playback.seek(0);
    setSelected(index);
  }
  return (
    <section
      className="wrap phux-explainer"
      id="how-it-works"
      aria-labelledby="explainer-title"
    >
      <header className="explainer-heading">
        <div>
          <p className="explainer-kicker">A runtime, not just a window.</p>
          <h2 id="explainer-title">Follow the work.</h2>
        </div>
        <p>
          Switch the scenario. Follow a signal. See what stays running when
          everything around it changes.
        </p>
      </header>
      <div
        className="scenario-picker"
        role="group"
        aria-label="Choose a scenario"
      >
        {SCENARIOS.map((item, index) => (
          <button
            key={item.id}
            type="button"
            aria-pressed={selected === index}
            onClick={() => selectScenario(index)}
          >
            <span>0{index + 1}</span>
            {item.label}
          </button>
        ))}
      </div>
      <div ref={stage} className="explainer-stage">
        <div className="stage-toolbar">
          <label>
            Client{" "}
            <select
              aria-label="Client"
              value={client}
              onChange={(event) => setClient(event.target.value)}
            >
              {CLIENTS.map((name) => (
                <option key={name}>{name}</option>
              ))}
            </select>
          </label>
          <button
            type="button"
            aria-pressed={technical}
            onClick={() => setTechnical((value) => !value)}
          >
            Protocol detail {technical ? "on" : "off"}
          </button>
        </div>
        <Diagram
          scenario={scenario}
          time={playback.time}
          client={client}
          technical={technical}
          reduced={reduced}
        />
        <Diagram
          scenario={scenario}
          time={playback.time}
          client={client}
          technical={technical}
          reduced={reduced}
          compact
        />
        <div className="playback-controls">
          <button type="button" onClick={playback.toggle} disabled={reduced}>
            {playback.playing ? "Pause" : "Play scenario"}
          </button>
          <label className="timeline-label">
            Timeline
            <input
              type="range"
              min="0"
              max={duration(scenario)}
              step="0.01"
              value={playback.time}
              onChange={(event) => playback.seek(Number(event.target.value))}
              aria-valuetext={`Step ${active + 1}: ${beat.title}`}
            />
          </label>
          <span className="step-counter">
            {active + 1} / {scenario.beats.length}
          </span>
        </div>
      </div>
      <div className="explainer-story">
        <div>
          <h3>{scenario.title}</h3>
          <p>{scenario.summary}</p>
          <a href={docsHref(scenario.href)}>
            Go deeper in the docs <span aria-hidden="true">→</span>
          </a>
        </div>
        <div>
          <div className="beat-picker" role="group" aria-label="Story steps">
            {scenario.beats.map((item, index) => (
              <button
                type="button"
                key={item.title}
                aria-pressed={active === index}
                aria-label={`Step ${index + 1}: ${item.title}`}
                onClick={() => playback.seek(index * BEAT_SECONDS)}
              >
                {index + 1}
              </button>
            ))}
          </div>
          <div
            className="beat-copy"
            aria-live={playback.playing ? "off" : "polite"}
            aria-atomic="true"
          >
            <h4>{beat.title}</h4>
            <p>{technical ? beat.detail : beat.text}</p>
          </div>
        </div>
      </div>
      <details
        className="architecture-film"
        onToggle={(event) => {
          if (!event.currentTarget.open)
            event.currentTarget.querySelector("video")?.pause();
        }}
      >
        <summary>
          The whole system in 16 seconds <span>Watch the Psychopomp film</span>
        </summary>
        <video
          controls
          playsInline
          preload="none"
          width="1920"
          height="1080"
          poster="/motion/phux-architecture-poster.jpg"
          aria-label="phux architecture film, silent"
          aria-describedby="film-description"
        >
          <source src="/motion/phux-architecture.mp4" type="video/mp4" />
          <a href="/motion/phux-architecture.mp4">
            Download the architecture film
          </a>
        </video>
        <p id="film-description">
          Clients and agents send structured input to the server. A PTY runs the
          program and emits VT bytes. Detaching a client leaves the server-owned
          terminal running. A federation hub exposes remote resources; their
          processes stay on their owning machines. The hub is a phux server
          role, not a required extra service.
        </p>
        <p>
          Silent film rendered with{" "}
          <a href="https://github.com/kitlangton/psychopomp">
            Kit Langton’s Psychopomp
          </a>
          . Timing is illustrative, not a latency benchmark.
        </p>
      </details>
      <p className="motion-note">
        {reduced
          ? "Still mode: choose steps or scrub the timeline. "
          : "Motion starts only when you press Play. "}
        Illustrative scenarios, not a live session. iPhone is in TestFlight
        beta.
      </p>
      <noscript>
        <p className="motion-note">
          Enable JavaScript for interactive scenarios, or follow the
          documentation link above for the full explanation.
        </p>
      </noscript>
    </section>
  );
}
