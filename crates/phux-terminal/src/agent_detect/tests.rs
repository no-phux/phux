use std::os::fd::RawFd;
use std::rc::Rc;
use std::time::{Duration, Instant};

use super::IdentitySource;
use super::identify::{Occupancy, Occupant, PaneOccupant};
use super::rules::{ManifestSpec, RuleSet};
use super::{
    AgentDetector, DetectOutcome, DetectedState, HOOK_IDENTITY_GRACE, IDLE_CONFIRMATIONS,
    STARTUP_GRACE, TICK_CONFIRMING, TICK_IDENTIFIED, VACANT_CONFIRMATIONS,
};

/// Reports this occupancy, with no PTY in sight.
struct Forced(Occupancy);

impl IdentitySource for Forced {
    fn pgid(&self, _: Option<RawFd>) -> Option<i32> {
        self.0.pgid()
    }

    fn observe(
        &self,
        _: Option<RawFd>,
        _: Option<i32>,
        _: &RuleSet,
    ) -> (Occupancy, Option<PaneOccupant>) {
        (self.0.clone(), None)
    }
}

/// A manifest exercising every arm of the hysteresis machine.
const MANIFEST: &str = r#"
kind = "t"
name = "t-agent"
binaries = ["t"]

[[rules]]
id = "working"
state = "working"
priority = 50
region = "viewport"
match = { contains = "WORKING" }

[[rules]]
id = "blocked"
state = "blocked"
priority = 80
region = "viewport"
match = { contains = "BLOCKED" }

[[rules]]
id = "done"
state = "done"
priority = 70
region = "viewport"
match = { contains = "DONE" }

[[rules]]
id = "idle-positive"
state = "idle"
priority = 40
region = "viewport"
visible-idle = true
match = { contains = "IDLE" }

[[rules]]
id = "pager"
priority = 200
region = "viewport"
skip-state-update = true
match = { contains = "PAGER" }
"#;

/// A second kind, so an occupant change is expressible.
const MANIFEST_OTHER: &str = r#"
kind = "u"
name = "u-agent"
binaries = ["u"]

[[rules]]
id = "working"
state = "working"
priority = 50
region = "viewport"
match = { contains = "WORKING" }
"#;

struct Harness {
    detector: AgentDetector,
    now: Instant,
    /// The actor's `agent_dirty_since_detect` flag.
    dirty: bool,
    /// The fake `AgentSession` child behind the live-session probe. Wired
    /// for every harness, so `false` also proves a quiet probe is inert.
    live_session: Rc<std::cell::Cell<bool>>,
}

impl Harness {
    fn new() -> Self {
        let mut h = Self::unidentified();
        let now = h.now;
        h.detector.force_identity("t", now);
        h
    }

    /// A pane that is still just a shell.
    fn unidentified() -> Self {
        let spec: ManifestSpec = toml::from_str(MANIFEST).expect("manifest parses");
        let other: ManifestSpec = toml::from_str(MANIFEST_OTHER).expect("manifest parses");
        let mut set = RuleSet::default();
        set.install(spec).expect("compiles");
        set.install(other).expect("compiles");
        let now = Instant::now();
        let live_session = Rc::new(std::cell::Cell::new(false));
        let mut detector = AgentDetector::new(Rc::new(set), now);
        let probe = Rc::clone(&live_session);
        detector.set_live_session_probe(Rc::new(move || probe.get()));
        Self {
            detector,
            now,
            dirty: false,
            live_session,
        }
    }

    fn past_grace(mut self) -> Self {
        self.now += STARTUP_GRACE + Duration::from_millis(1);
        self
    }

    /// Replay the actor's real `detect_tick` ordering: `wants_screen` is
    /// asked before identity resolves, and the dirty flag is consumed only
    /// when a scan happens.
    fn actor_tick(&mut self, screen: &str) -> DetectOutcome {
        self.now += self.detector.interval();
        let lines = [screen.to_owned()];
        let scan = self.detector.wants_screen(self.dirty);
        if scan {
            self.dirty = false;
        }
        self.detector
            .tick(self.now, None, "", "", scan.then_some(&lines[..]))
    }

    /// A human launches an agent at the shell prompt; it paints.
    fn launch_agent(&mut self, kind: &str) {
        self.occupy(agent(kind, 100));
        self.dirty = true;
    }

    /// Set what the kernel reports about the foreground group.
    fn occupy(&mut self, occupancy: Occupancy) {
        self.detector.identity = Box::new(Forced(occupancy));
    }

    /// Make the next tick's identity poll due.
    fn poll_identity_now(&mut self) {
        self.detector.next_identify = self.now;
    }

    /// A tick whose identity poll runs, with a `WORKING` screen.
    fn identity_tick(&mut self) -> DetectOutcome {
        self.poll_identity_now();
        self.tick("WORKING")
    }

    /// Advance by the detector's interval and feed it a screen.
    fn tick(&mut self, screen: &str) -> DetectOutcome {
        self.now += self.detector.interval();
        let lines = vec![screen.to_owned()];
        self.detector.tick(self.now, None, "", "", Some(&lines))
    }

    /// Tick with the scan skipped (the cheap steady state).
    fn tick_no_scan(&mut self) -> DetectOutcome {
        self.now += self.detector.interval();
        self.detector.tick(self.now, None, "", "", None)
    }

    fn state(&self) -> Option<DetectedState> {
        self.detector.published.as_ref().map(|r| r.state)
    }

    fn session_open(&self) {
        self.live_session.set(true);
    }

    fn session_end(&self) {
        self.live_session.set(false);
    }

    /// The live child's stream publishes `state` at rank `Stream`.
    fn stream_says(&mut self, state: DetectedState) {
        let now = self.now;
        self.detector.report_stream_state(Some(state), now);
    }
}

/// A live agent occupant; the start time is derived from the pgid.
fn agent(kind: &str, pgid: i32) -> Occupancy {
    let started = u64::try_from(pgid).unwrap_or(0).saturating_mul(7);
    agent_started(kind, pgid, Some(started))
}

/// As [`agent`], but stating the start time outright.
fn agent_started(kind: &str, pgid: i32, started: Option<u64>) -> Occupancy {
    Occupancy::Agent {
        kind: kind.to_owned(),
        occupant: Occupant::new(pgid, started),
    }
}

fn published(outcome: &DetectOutcome) -> DetectedState {
    match outcome {
        DetectOutcome::Publish(report) => report.state,
        other => panic!("expected a publish, got {other:?}"),
    }
}

/// Acquire through the real identity path and publish `working`.
fn occupied(kind: &str, pgid: i32) -> Harness {
    let mut h = Harness::unidentified();
    h.occupy(agent(kind, pgid));
    assert_eq!(h.identity_tick(), DetectOutcome::Quiet, "acquiring");
    h.now += STARTUP_GRACE + Duration::from_millis(1);
    assert_eq!(published(&h.tick("WORKING")), DetectedState::Working);
    h
}

mod launch;
mod occupancy;
mod pgid;
mod publish;
mod stream;
