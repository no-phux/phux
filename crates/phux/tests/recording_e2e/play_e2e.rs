//! `phux play` as a pane (ADR-0064): a `.cast`'s bytes reach a real pane's
//! grid, read back through `phux snapshot --json`, and the TARGET pane is
//! never written to. Waits are polls with hang ceilings; the loop test counts
//! repaints via `phux rec` rather than measuring time.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// The seed pane: the one pane every server here starts with.
const SEED_PANE: &str = "@1";

/// The grid a pane gets with nobody attached; the fixture sizes differ.
const NO_TTY_DEFAULT: (u64, u64) = (80, 24);

/// The fixture's header grid.
const FIXTURE_HEADER: (u64, u64) = (100, 30);

/// The grid the fixture's mid-stream `r` event asks for.
const FIXTURE_RESIZED: (u64, u64) = (64, 18);

/// Text the fixture paints before its resize event.
const MARKER_ONE: &str = "PHUX-PLAYBACK-MARKER-ONE";

/// Text the fixture paints after its resize event.
const MARKER_TWO: &str = "PHUX-PLAYBACK-MARKER-TWO";

/// The fixture's bare-line-feed probe: `LFCOL`, a lone `\n`, then `X`.
const LF_PROBE: &str = "LFCOL";

/// The probe's next row: the `X` keeps its column.
const LF_PROBE_NEXT_ROW: &str = "     X";

/// Hang detector for every "the pane reached this state" poll.
const STATE_DEADLINE: Duration = Duration::from_secs(45);

/// Poll cadence for every wait loop in this file.
const POLL: Duration = Duration::from_millis(100);

/// Monotonic counter so concurrent tests never collide on a socket path.
static COUNTER: AtomicU32 = AtomicU32::new(0);

/// The committed demo recording, a real 80x24 phux session.
fn demo_cast() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/assets/recording-demo.cast")
        .canonicalize()
        .expect("the committed demo cast must exist")
}

/// A committed asciicast v3 recording made by asciinema (relative event
/// intervals, grid nested under `term`).
fn v3_cast() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/assets/pi-live-fleet.cast")
        .canonicalize()
        .expect("the committed v3 cast must exist")
}

const V3_HEADER: (u64, u64) = (140, 40);

/// The fixture: a 100x30 header, a marker, a resize to 64x18, a second
/// marker, and the line-feed probe.
fn fixture_cast() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/play-fit.cast")
        .canonicalize()
        .expect("the play fixture must exist")
}

/// A running `phux server`, killed and unlinked when the guard drops.
struct ServerGuard(common::ServerGuard);

impl std::ops::Deref for ServerGuard {
    type Target = common::ServerGuard;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl ServerGuard {
    fn start() -> Self {
        Self(common::ServerGuard::start("play"))
    }

    /// Start a playback (`phux play FILE [TARGET]`) and return its pane.
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

    /// A pane's `(cols, rows, lines)`, or `None` once it is gone.
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

    fn title(&self, pane: &str) -> Option<String> {
        let (code, stdout, _) = self.run(&["snapshot", "--json", pane]);
        if code != 0 {
            return None;
        }
        let doc: serde_json::Value = serde_json::from_str(&stdout).expect("snapshot must be JSON");
        doc["title"].as_str().map(str::to_owned)
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
    // Speed scales deadlines only, so the final screen is the real-time one.
    let pane = server.play(&["--speed", "50"], &demo_cast(), Some(SEED_PANE));
    assert_ne!(pane, SEED_PANE, "playback must create its own pane");

    let recorded = [
        "recdemo: 1 window",
        "$ phux rec work -o /tmp/inner.cast --duration 6",
        "phux: wrote /tmp/inner.gif (5.6 KiB, 5 frames, 3.8s)",
    ];
    for line in recorded {
        server.wait_for(&pane, line, |server, pane| server.text(pane).contains(line));
    }
    assert_eq!(server.size(&pane), Some(NO_TTY_DEFAULT));

    // TARGET says where the pane goes, never what is overwritten (ADR-0064).
    let target = server.text(SEED_PANE);
    for line in recorded {
        assert!(
            !target.contains(line),
            "leaked {line:?} into TARGET: {target:?}"
        );
    }
    assert_eq!(
        server.size(SEED_PANE),
        Some(NO_TTY_DEFAULT),
        "fitting the playback pane must not resize TARGET"
    );

    // The writer sets the ended title after the final event, then holds.
    server.wait_for(&pane, "the playback's ended title", |server, pane| {
        server
            .title(pane)
            .is_some_and(|title| title.ends_with(" (ended)"))
    });
    assert!(
        server.screen(&pane).is_some(),
        "the playback pane must hold its final frame until it is killed"
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

    // Real speed: the fixture's 3s hold makes both grids observable.
    let pane = server.play(&[], &fixture_cast(), None);

    server.wait_for(&pane, "the recording's header grid", |server, pane| {
        server.size(pane) == Some(FIXTURE_HEADER)
    });
    server.wait_for(&pane, "the recorded resize", |server, pane| {
        server.size(pane) == Some(FIXTURE_RESIZED)
    });
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
    // Wait on the probe's next row itself: other markers also contain `X`.
    server.wait_for(&pane, "the probe's second row", |server, pane| {
        server.screen(pane).is_some_and(|(_, _, lines)| {
            lines
                .iter()
                .position(|line| line.contains(LF_PROBE))
                .and_then(|row| lines.get(row + 1))
                .is_some_and(|next| !next.trim().is_empty())
        })
    });

    // A bare line feed keeps the column; column 0 would mean `ONLCR`
    // rewrote the recording (the writer clears `OPOST` to prevent it).
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
    let pane = server.play(&["--no-fit", "--idle-limit", "0.2"], &fixture_cast(), None);
    server.wait_for(&pane, "the end of the recording", |server, pane| {
        server.text(pane).contains(MARKER_TWO)
    });

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
    // Passes are counted from a `phux rec` observer: one marker per pass.
    let pane = server.play(
        &["--loop", "--speed", "3", "--idle-limit", "0.2"],
        &fixture_cast(),
        None,
    );
    let out = std::env::temp_dir().join(format!(
        "phux-play-loop-{}-{}.cast",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    server.success(&[
        "rec",
        &pane,
        "-o",
        &out.to_string_lossy(),
        "--duration",
        "1",
    ]);
    let recorded = std::fs::read_to_string(&out).expect("the recording of the playback");
    let passes = recorded.matches(MARKER_TWO).count();
    assert!(
        passes >= 3,
        "--loop must replay the recording at least three times; the observer \
         saw the final marker {passes} time(s) in {} bytes",
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
    // 3s gap clamped to 0.5s, plus 200ms of tail, halved by --speed 2.
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
