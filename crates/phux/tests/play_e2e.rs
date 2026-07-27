//! Binary-level end-to-end tests for `phux play`: playback as a pane
//! (ADR-0064).
//!
//! What these prove that a unit test cannot: the bytes in a `.cast` end up
//! on a **real pane's grid**, as read back through `phux snapshot --json` —
//! i.e. through the server's own libghostty `Terminal`, not through anything
//! this feature wrote. The chain under test is long and every link is real: a
//! `SPAWN_TERMINAL` whose command is the phux binary in writer mode, a PTY,
//! the line discipline, the pane actor's reader, and the emulator. A unit
//! test can check the argv; only this can check that the argv produced a
//! screen.
//!
//! The other thing only a live server can show is the *negative*: the pane
//! named by TARGET is never written to. Playback creates a pane beside it,
//! and `never_writes_into_the_target_pane` reads the target's grid back to
//! prove nothing leaked into it.
//!
//! Harness discipline follows `resize_e2e.rs`: a real `phux server` child on
//! a private UDS at the root of `/tmp` (macOS caps `sun_path` at 104 bytes
//! and these are usually run by hand from a deep worktree), each verb its own
//! subprocess, guard-killed and unlinked on drop.
//!
//! Timing discipline: the fixture is played at real speed and every wait is a
//! poll with a HANG ceiling, never a `sleep` sized to what the recording
//! "should" take. The one place a deadline is load-bearing is
//! `loop_replays_the_recording_more_than_once`, and it does not measure time
//! at all — it counts how many times the marker was painted, using
//! `phux rec` as the observer.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// Idle lifetime for this file's harness servers, as a backstop UNDER the
/// `Drop` kill (ADR-0063). The guard is still the primary cleanup; it cannot
/// run if the test process is `SIGKILL`ed, and what leaks then is a daemon
/// holding a parked playback pane forever — this file creates panes that by
/// design never exit on their own, so it is the worst file in the tree to
/// leak from.
const SERVER_IDLE_LIMIT_SECS: &str = "600";

/// Path to the freshly-built `phux` binary, injected by cargo.
const PHUX: &str = env!("CARGO_BIN_EXE_phux");

/// The pre-seeded session name every test drives against.
const SESSION: &str = "work";

/// The seed pane's id. Every server here starts with exactly one pane, so
/// this is the pane `.` resolves to and the pane playback is placed beside.
const SEED_PANE: &str = "@1";

/// The grid a pane gets with nobody attached. Every geometry assertion below
/// is written against a size that is NOT this, so none can pass by accident.
const NO_TTY_DEFAULT: (u64, u64) = (80, 24);

/// The fixture's header grid — deliberately unlike [`NO_TTY_DEFAULT`].
const FIXTURE_HEADER: (u64, u64) = (100, 30);

/// The grid the fixture's mid-stream `r` event asks for — unlike both of the
/// above, so observing it can only mean the resize event was honored.
const FIXTURE_RESIZED: (u64, u64) = (64, 18);

/// Text the fixture paints before its resize event.
const MARKER_ONE: &str = "PHUX-PLAYBACK-MARKER-ONE";

/// Text the fixture paints after its resize event.
const MARKER_TWO: &str = "PHUX-PLAYBACK-MARKER-TWO";

/// The fixture's bare-line-feed probe: five columns of text, a lone `\n`
/// (NOT `\r\n`), then one more character. See
/// `recorded_bytes_reach_the_pane_untranslated`.
const LF_PROBE: &str = "LFCOL";

/// What the probe must paint on the following row: the `X` stays in the
/// column the line feed left it in, five cells across.
const LF_PROBE_NEXT_ROW: &str = "     X";

/// How long to wait for the server to bind its socket (cold-start bound).
const SOCKET_DEADLINE: Duration = Duration::from_secs(30);

/// Ceiling on any "the pane reached this state" poll.
///
/// A HANG detector, not a timing gate: the fixture's own timeline is 3.1
/// seconds, so this is an order of magnitude above the real number and can
/// only elapse if the state is never coming.
const STATE_DEADLINE: Duration = Duration::from_secs(45);

/// Poll cadence for every wait loop in this file.
const POLL: Duration = Duration::from_millis(100);

/// Monotonic counter so concurrent tests never collide on a socket path.
static COUNTER: AtomicU32 = AtomicU32::new(0);

/// The committed demo recording — a real 80x24 phux session, not a
/// synthetic file. Playing it is the closest this lane gets to what a user
/// will actually do.
fn demo_cast() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/assets/recording-demo.cast")
        .canonicalize()
        .expect("the committed demo cast must exist")
}

/// The committed asciicast **v3** recording that backs
/// `docs/pi-live-fleet-proof.md`. Produced by asciinema itself, not by phux,
/// which is what makes it worth playing: v3 stores event times as relative
/// intervals rather than absolute offsets.
fn v3_cast() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/assets/pi-live-fleet.cast")
        .canonicalize()
        .expect("the committed v3 cast must exist")
}

/// That recording's grid, which v3 nests under `term` rather than putting
/// flat on the header.
const V3_HEADER: (u64, u64) = (140, 40);

/// The purpose-built fixture: a 100x30 header, a marker, a mid-stream resize
/// to 64x18, and a second marker. Three geometries that cannot be confused
/// with each other, which is what makes the fit assertions falsifiable.
fn fixture_cast() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/play-fit.cast")
        .canonicalize()
        .expect("the play fixture must exist")
}

/// A running `phux server`, killed and unlinked when the guard drops.
struct ServerGuard {
    child: Child,
    socket: PathBuf,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // The server unlinks on clean shutdown; we SIGKILL it, so a stale
        // socket in /tmp would otherwise outlive every run.
        let _ = std::fs::remove_file(&self.socket);
    }
}

impl ServerGuard {
    fn start() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let socket = PathBuf::from(format!(
            "/tmp/phux-play-e2e-{}-{n}.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&socket);
        let child = Command::new(PHUX)
            .args(["server", "--session", SESSION, "--socket"])
            .arg(&socket)
            .args(["--exit-after-idle", SERVER_IDLE_LIMIT_SECS])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn phux server");
        let guard = Self { child, socket };
        let deadline = Instant::now() + SOCKET_DEADLINE;
        while Instant::now() < deadline {
            if guard.socket.exists() {
                return guard;
            }
            std::thread::sleep(POLL);
        }
        panic!(
            "phux server did not bind {} within {SOCKET_DEADLINE:?}",
            guard.socket.display()
        );
    }

    /// Build `phux <verb> --socket <sock> <rest...>`.
    fn cmd(&self, args: &[&str]) -> Command {
        let (verb, rest) = args.split_first().expect("at least a verb");
        let mut c = Command::new(PHUX);
        c.arg(verb)
            .arg("--socket")
            .arg(&self.socket)
            .args(rest)
            .stdin(Stdio::null());
        c
    }

    /// Run a verb, returning `(exit code, stdout, stderr)`.
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = self.cmd(args).output().expect("run phux verb");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn success(&self, args: &[&str]) -> String {
        let (code, stdout, stderr) = self.run(args);
        assert_eq!(code, 0, "phux {args:?} exited {code}; stderr={stderr}");
        stdout
    }

    /// Start a playback and return the pane it created, from `--json`.
    ///
    /// The positional order is the verb's own — `phux play FILE [TARGET]` —
    /// so `target` goes after the cast, not into `extra`.
    fn play(&self, extra: &[&str], cast: &Path, target: Option<&str>) -> String {
        let cast = cast.to_string_lossy().into_owned();
        let mut args = vec!["play", "--json"];
        args.extend_from_slice(extra);
        args.push(&cast);
        if let Some(target) = target {
            args.push(target);
        }
        let stdout = self.success(&args);
        let doc: serde_json::Value =
            serde_json::from_str(&stdout).expect("play --json must be JSON");
        let id = doc["terminal_id"].as_u64().expect("terminal_id");
        format!("@{id}")
    }

    /// A pane's screen as `GET_SCREEN` projects it: `(cols, rows, lines)`.
    ///
    /// `None` when the pane is gone, which is how the `--close` test tells
    /// "playback ended and closed" from "playback is still running".
    fn screen(&self, pane: &str) -> Option<(u64, u64, Vec<String>)> {
        let (code, stdout, _) = self.run(&["snapshot", "--json", pane]);
        if code != 0 {
            return None;
        }
        let doc: serde_json::Value = serde_json::from_str(&stdout).expect("snapshot must be JSON");
        Some((
            doc["cols"].as_u64().expect("cols"),
            doc["rows"].as_u64().expect("rows"),
            doc["lines"]
                .as_array()
                .expect("lines")
                .iter()
                .map(|line| line.as_str().unwrap_or_default().to_owned())
                .collect(),
        ))
    }

    fn size(&self, pane: &str) -> Option<(u64, u64)> {
        self.screen(pane).map(|(cols, rows, _)| (cols, rows))
    }

    fn text(&self, pane: &str) -> String {
        self.screen(pane)
            .map(|(_, _, lines)| lines.join("\n"))
            .unwrap_or_default()
    }

    /// Poll until `predicate` holds, or panic naming what was seen last.
    fn wait_for(&self, pane: &str, what: &str, predicate: impl Fn(&Self, &str) -> bool) {
        let deadline = Instant::now() + STATE_DEADLINE;
        while Instant::now() < deadline {
            if predicate(self, pane) {
                return;
            }
            std::thread::sleep(POLL);
        }
        panic!(
            "pane {pane} never reached {what} within {STATE_DEADLINE:?}; last size={:?} text={:?}",
            self.size(pane),
            self.text(pane)
        );
    }
}

#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn playback_paints_the_recorded_screen_into_a_real_pane() {
    let server = ServerGuard::start();
    // Played fast, because this assertion is about *what* landed on the
    // grid, not when. Speed scales the deadlines and nothing else — no event
    // is dropped, merged, or resampled — so the final screen is identical to
    // the one a real-time playback would leave.
    let pane = server.play(&["--speed", "50"], &demo_cast(), None);

    // These are lines the demo recording actually painted, read back out of
    // the pane's own libghostty grid. Nothing in the playback path invents
    // them and nothing but a working PTY write can put them there.
    for line in [
        "recdemo: 1 window",
        "$ phux rec work -o /tmp/inner.cast --duration 6",
        "phux: wrote /tmp/inner.gif (5.6 KiB, 5 frames, 3.8s)",
    ] {
        server.wait_for(&pane, line, |server, pane| server.text(pane).contains(line));
    }

    // The recording is 80x24 and so is a no-TTY pane, so this says only that
    // the fit did not *break* anything. The fixture test is what proves the
    // fit moves a grid.
    assert_eq!(server.size(&pane), Some(NO_TTY_DEFAULT));

    // And the pane is still there after the last byte. A pane that erased
    // itself would make every assertion above a race, and would make the
    // whole feature unobservable.
    std::thread::sleep(Duration::from_secs(2));
    assert!(
        server.screen(&pane).is_some(),
        "the playback pane must hold its final frame until it is killed"
    );
}

#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn never_writes_into_the_target_pane() {
    let server = ServerGuard::start();
    let pane = server.play(&["--speed", "50"], &demo_cast(), Some(SEED_PANE));
    assert_ne!(pane, SEED_PANE, "playback must create its own pane");
    server.wait_for(&pane, "the recording's first line", |server, pane| {
        server.text(pane).contains("recdemo: 1 window")
    });

    // TARGET says WHERE the playback pane goes, never what gets overwritten
    // (ADR-0064 decision 4). The seed pane has a live shell in it; if
    // playback could reach an existing pane, this is where the recording's
    // text would show up.
    //
    // Stated as "none of the recording's lines appear here" rather than
    // "this pane's screen never changed": the target holds a real
    // interactive shell, which paints its own prompt on its own schedule,
    // and an equality assertion would be testing the user's zsh config.
    let target = server.text(SEED_PANE);
    for line in [
        "recdemo: 1 window",
        "$ phux rec work -o /tmp/inner.cast --duration 6",
        "phux: wrote /tmp/inner.gif (5.6 KiB, 5 frames, 3.8s)",
    ] {
        assert!(
            !target.contains(line),
            "playback leaked {line:?} into the TARGET pane: {target:?}"
        );
    }
    assert_eq!(
        server.size(SEED_PANE),
        Some(NO_TTY_DEFAULT),
        "fitting the playback pane to the recording must not resize TARGET"
    );
}

#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn the_pane_is_fitted_to_the_recording_and_to_its_resize_events() {
    let server = ServerGuard::start();
    assert_eq!(
        server.size(SEED_PANE),
        Some(NO_TTY_DEFAULT),
        "a freshly seeded pane must start at the no-TTY default, or the \
         geometry assertions below could pass without a resize happening"
    );

    // Real speed: the fixture holds 100x30 for three seconds before its
    // resize event, and that hold is what makes the two states observable in
    // sequence rather than as a single end state.
    let pane = server.play(&[], &fixture_cast(), None);

    // 1. The header's grid, applied before the first byte — this is the fit
    //    that keeps a recording from wrapping in the wrong places.
    server.wait_for(&pane, "the recording's header grid", |server, pane| {
        server.size(pane) == Some(FIXTURE_HEADER)
    });
    // 2. The `r` event's grid. Neither 80x24 nor 100x30, so nothing but the
    //    recorded resize can produce it.
    server.wait_for(&pane, "the recorded resize", |server, pane| {
        server.size(pane) == Some(FIXTURE_RESIZED)
    });
    // 3. The bytes *after* the resize, which arrive 100 ms behind it —
    //    waited for rather than assumed, because reading the grid the
    //    instant the size changes is a race this test lost once already.
    server.wait_for(&pane, "the post-resize marker", |server, pane| {
        server.text(pane).contains(MARKER_TWO)
    });

    let text = server.text(&pane);
    assert!(
        text.contains(MARKER_ONE),
        "content painted before the resize must survive it: {text:?}"
    );
    assert_eq!(
        server.size(&pane),
        Some(FIXTURE_RESIZED),
        "the recorded resize must still hold once the recording ends"
    );
}

#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn recorded_bytes_reach_the_pane_untranslated() {
    let server = ServerGuard::start();
    let pane = server.play(&["--idle-limit", "0.2"], &fixture_cast(), None);
    // Waited for as "the row after the probe is non-empty", NOT as "some row
    // contains an X": every other marker on this screen contains one (`PHUX`),
    // so the obvious predicate is satisfied before the probe has painted
    // anything and the assertion below then reads a blank row. That is
    // exactly how this test failed under parallel load the first time.
    server.wait_for(&pane, "the probe's second row", |server, pane| {
        server.screen(pane).is_some_and(|(_, _, lines)| {
            lines
                .iter()
                .position(|line| line.contains(LF_PROBE))
                .and_then(|row| lines.get(row + 1))
                .is_some_and(|next| !next.trim().is_empty())
        })
    });

    // The probe writes `LFCOL`, then a BARE line feed, then `X`. A line feed
    // moves down a row and leaves the column alone, so the `X` must land in
    // column 5. If it lands in column 0, the pane's line discipline rewrote
    // the recording's `\n` into `\r\n` on the way through (`ONLCR`) and
    // playback is delivering a translation of the cast rather than the cast.
    // That is the entire reason the in-pane writer clears `OPOST`, and it is
    // invisible to every other assertion in this file because ordinary
    // recorded output already carries its own carriage returns.
    let (_, _, lines) = server.screen(&pane).expect("the pane is alive");
    let probe_row = lines
        .iter()
        .position(|line| line.contains(LF_PROBE))
        .expect("the probe row");
    assert_eq!(
        lines.get(probe_row + 1).map(String::as_str),
        Some(LF_PROBE_NEXT_ROW),
        "a bare line feed must not have returned the carriage; screen={lines:?}"
    );
}

#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn an_asciicast_v3_recording_plays_too() {
    let server = ServerGuard::start();
    // v3 is not a variant of v2: its header nests the grid under `term` and
    // its event times are relative intervals, so a reader that tolerated a
    // v3 header while treating the intervals as absolute offsets would play
    // a four-minute recording in a fraction of a second. Both halves are
    // observable here — the grid, because the fit uses the parsed header,
    // and the timebase, because a mis-read one would finish before the first
    // poll rather than painting progressively.
    let pane = server.play(&["--speed", "50", "--idle-limit", "0.1"], &v3_cast(), None);
    server.wait_for(&pane, "the v3 header's grid", |server, pane| {
        server.size(pane) == Some(V3_HEADER)
    });
    server.wait_for(&pane, "painted output", |server, pane| {
        server
            .screen(pane)
            .is_some_and(|(_, _, lines)| lines.iter().any(|line| !line.trim().is_empty()))
    });
}

#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn no_fit_leaves_the_grid_alone() {
    let server = ServerGuard::start();
    // `--idle-limit` collapses the fixture's three-second hold, because this
    // test only cares about the end state.
    let pane = server.play(&["--no-fit", "--idle-limit", "0.2"], &fixture_cast(), None);
    server.wait_for(&pane, "the end of the recording", |server, pane| {
        server.text(pane).contains(MARKER_TWO)
    });

    // Both the header fit AND the mid-stream resize event are suppressed: a
    // caller that pinned the grid meant it, and honoring one but not the
    // other would be the worst of both.
    assert_eq!(
        server.size(&pane),
        Some(NO_TTY_DEFAULT),
        "--no-fit must suppress the header fit and the recorded resize alike"
    );
}

#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn close_ends_the_pane_when_playback_ends() {
    let server = ServerGuard::start();
    let pane = server.play(&["--close", "--idle-limit", "0.2"], &fixture_cast(), None);
    let deadline = Instant::now() + STATE_DEADLINE;
    while Instant::now() < deadline {
        if server.screen(&pane).is_none() {
            return;
        }
        std::thread::sleep(POLL);
    }
    panic!("--close must end the pane when the recording does; {pane} is still alive");
}

#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn loop_replays_the_recording_more_than_once() {
    let server = ServerGuard::start();
    // Counted, not timed. `phux rec` subscribes to the playback pane as a
    // pure observer and writes every byte it sees into a cast; the marker
    // appears once per pass, so the count in the recording of the playback
    // is a hard statement about how many passes ran. A stopwatch would only
    // say "it took a while", which a slow box says too.
    let pane = server.play(
        &["--loop", "3", "--speed", "3", "--close"],
        &fixture_cast(),
        None,
    );
    let out = std::env::temp_dir().join(format!(
        "phux-play-loop-{}-{}.cast",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    // `phux rec` stops early when the pane exits, so `--duration` here is
    // only a ceiling on a playback that never ends.
    server.success(&[
        "rec",
        &pane,
        "-o",
        &out.to_string_lossy(),
        "--duration",
        "60",
    ]);
    let recorded = std::fs::read_to_string(&out).expect("the recording of the playback");
    let passes = recorded.matches(MARKER_TWO).count();
    assert!(
        passes >= 2,
        "--loop 3 must replay the recording; the observer saw the final \
         marker {passes} time(s) in {} bytes",
        recorded.len()
    );
    let _ = std::fs::remove_file(&out);
}

#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn a_file_that_is_not_a_cast_fails_before_any_pane_is_created() {
    let server = ServerGuard::start();
    let junk = std::env::temp_dir().join(format!("phux-play-junk-{}.cast", std::process::id()));
    std::fs::write(&junk, b"not an asciicast\n").expect("write junk");

    let (code, stdout, stderr) = server.run(&["play", &junk.to_string_lossy()]);
    assert_eq!(code, 1, "a malformed cast must fail; stdout={stdout}");
    assert!(
        stderr.contains("not-a-cast") || stderr.contains(&junk.display().to_string()),
        "the diagnostic must name the file: {stderr}"
    );
    // Validation happens in the caller's terminal, before the spawn — the
    // failure must not leave a pane behind that flashed an error at a grid
    // nobody was watching.
    assert_eq!(
        server.size("@2"),
        None,
        "a rejected cast must not have created a pane"
    );

    // A path that does not exist at all fails the same way.
    let (code, _, stderr) = server.run(&["play", "/nonexistent/nope.cast"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("nope.cast"), "{stderr}");
    let _ = std::fs::remove_file(&junk);
}

#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn json_names_the_pane_and_the_recording() {
    let server = ServerGuard::start();
    let cast = fixture_cast();
    let stdout = server.success(&[
        "play",
        "--json",
        "--speed",
        "2",
        "--idle-limit",
        "0.5",
        &cast.to_string_lossy(),
    ]);
    let doc: serde_json::Value = serde_json::from_str(&stdout).expect("play --json must be JSON");

    assert_eq!(doc["terminal_id"], 2, "the pane created for the playback");
    assert_eq!(doc["cols"], FIXTURE_HEADER.0);
    assert_eq!(doc["rows"], FIXTURE_HEADER.1);
    assert_eq!(doc["events"], 5);
    assert_eq!(doc["passes"], 1);
    assert_eq!(doc["idle_limit"], 0.5);
    // The fixture's 3s gap clamped to 0.5s, then 200ms of tail events,
    // halved by --speed 2: 350ms. The reported duration is the wait the
    // caller is actually in for, which is the only version worth printing.
    assert_eq!(doc["duration_ms"], 350);
    assert_eq!(
        doc["path"],
        cast.to_string_lossy().into_owned(),
        "the path is absolute, because the pane's child resolves it from the \
         daemon's cwd and not the caller's"
    );

    // The pane it named is the pane that plays.
    server.wait_for("@2", "the fixture's final marker", |server, pane| {
        server.text(pane).contains(MARKER_TWO)
    });
}
