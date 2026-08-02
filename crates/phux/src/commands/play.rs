//! `phux play` — replay a recording *as a pane* (ADR-0064).
//!
//! This is deliberately not `asciinema play`. That command paints a cast
//! onto whatever terminal you happen to be sitting in and leaves nothing
//! behind; this one creates a **Terminal in the multiplexer whose PTY is fed
//! from the cast**. The result is an ordinary pane: attachable, snapshotable
//! with `phux snapshot`, resizable with `phux resize`, observable over
//! `ATTACH_TERMINAL` by an agent, shareable with a second client, and
//! killable with `phux kill`. Nothing else can build that, which is the only
//! reason this verb exists — see ADR-0064 for the boundary and ADR-0060 for
//! the scope line it moves.
//!
//! # Two processes, one verb
//!
//! [`run_play`] runs in one of two modes:
//!
//! * **launcher** (what a user invokes) — validate the cast, then
//!   `SPAWN_TERMINAL` a pane whose *command* is this same binary in writer
//!   mode. It never touches the pane it was pointed at; `TARGET` names where
//!   the new pane goes, never what gets overwritten.
//! * **writer** (`--pty-writer`, hidden) — the process running *inside* that
//!   pane. Its stdout is the PTY, so writing the cast's `o` bytes to stdout
//!   is exactly "feeding the PTY from the cast", and the server's existing
//!   read path forwards them on the wire like any other program's output.
//!
//! The argv that joins them is built and consumed by the same binary, so
//! there is no compatibility surface between the two halves and the flag
//! stays hidden. A different `phux` version cannot end up on either end: the
//! launcher spawns its own [`std::env::current_exe`].
//!
//! # Zero wire change
//!
//! `SPAWN_TERMINAL` already carries a `command`, the server already injects
//! `PHUX_TERMINAL_ID` + `PHUX_SOCKET` into every spawned pane, and
//! `TERMINAL_RESIZE` already exists (ADR-0062). Playback therefore adds no
//! frame, no command tag, no `ServerFeature` bit, and no version bump — which
//! under ADR-0061's hard `major.minor` gate is not a nicety but the
//! difference between shipping this and breaking every client in the fleet.

use std::io::BufReader;
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use phux_protocol::ids::TerminalId;
use phux_protocol::wire::frame::{FrameKind, SpawnResult};
use phux_record::cast::{CastEvent, CastHeader, EventCode, read_cast};
use phux_record::playback::{Speed, due_at, pass_duration};
use phux_record::timeline::clamp_idle;
use phux_server::runtime::default_socket_path;

use crate::commands::{SpawnSplit, cli_runtime, resize::parse_geometry};

/// Sequence written between loop iterations: soft reset (DECSTR), leave the
/// alternate screen, erase the screen *and* the scrollback, home the cursor.
///
/// Not `RIS` (`ESC c`), the sledgehammer a naive player reaches for: a full
/// reset also drops the pane's grid size back to whatever the emulator
/// considers default, which would undo the fit performed for this exact
/// recording and make the second pass of a `--loop` wrap differently from
/// the first. DECSTR clears the modes a recording can leave set (origin
/// mode, insert mode, a scroll region) without touching geometry.
///
/// The explicit `?1049l` is there because DECSTR does not leave the
/// alternate screen: a recording of `vim` or `htop` ends *inside* it, and
/// without this the next pass would replay onto the alt buffer and the
/// recording's own `?1049h` would then have nothing to restore.
const LOOP_RESET: &[u8] = b"\x1b[!p\x1b[?1049l\x1b[2J\x1b[3J\x1b[H";

/// How long the writer sleeps between checks while holding the final frame.
///
/// Long, because there is nothing to check: the process is parked so the
/// pane keeps existing, and every wakeup is pure overhead on a box that may
/// be holding dozens of these.
const HOLD_TICK: Duration = Duration::from_secs(3600);

/// Everything `phux play` was asked to do.
///
/// A struct rather than a dozen positional parameters, for the reason
/// `RecArgs` gives: `clippy::too_many_arguments` is on and a call site with
/// twelve bare values is unreadable regardless.
#[derive(Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one field per CLI flag; collapsing the four booleans into enums would put a translation layer between `--help` and this struct for no reader's benefit"
)]
pub(crate) struct PlayArgs<'a> {
    /// The `.cast` to play.
    pub(crate) file: &'a Path,
    /// Where the playback pane goes; `None` means beside the focused pane.
    pub(crate) target: Option<&'a str>,
    /// Wall-clock divisor for the recorded timeline.
    pub(crate) speed: Speed,
    /// Collapse pauses longer than this; `None` means "use the header's".
    pub(crate) idle_limit: Option<f64>,
    /// Passes to play; `None` means until the pane is killed.
    pub(crate) passes: Option<u32>,
    /// Split axis for the new pane.
    pub(crate) split: SpawnSplit,
    /// Split ratio for the new pane.
    pub(crate) ratio: f32,
    /// Leave the pane's grid alone instead of fitting it to the recording.
    pub(crate) no_fit: bool,
    /// Close the pane when playback ends instead of holding the last frame.
    pub(crate) close: bool,
    /// Emit one JSON object on stdout instead of the human one-liner.
    pub(crate) json: bool,
    /// Override the UDS path.
    pub(crate) socket: Option<PathBuf>,
    /// Internal: this process *is* the pane. Never set by a user.
    pub(crate) pty_writer: bool,
}

/// A cast, loaded and normalized: idle-clamped once, on the shared list,
/// exactly as `phux rec` does before it writes and renders.
struct Loaded {
    header: CastHeader,
    events: Vec<CastEvent>,
    /// The clamp that was applied, for the report. `None` = none applied.
    idle_limit: Option<f64>,
}

/// Run `phux play` in whichever of its two modes the argv selected.
pub(crate) fn run_play(args: &PlayArgs<'_>) -> ExitCode {
    if args.pty_writer {
        return run_writer(args);
    }
    run_launcher(args)
}

// ---------------------------------------------------------------- launcher

/// The user-facing half: validate, spawn a pane that plays, report it.
///
/// Validation happens *here*, in the caller's own terminal, and not in the
/// pane. A typo'd path or a truncated cast must be a plain stderr line and a
/// nonzero exit — not a pane that appears, prints an error into a grid
/// nobody is looking at, and vanishes.
fn run_launcher(args: &PlayArgs<'_>) -> ExitCode {
    // Absolute, because the pane's child does not inherit this process's
    // working directory: it is spawned by the daemon, which is `setsid`'d
    // away and may be running from `/`. A relative path that resolves here
    // would resolve to nothing there.
    let file = match std::fs::canonicalize(args.file) {
        Ok(path) => path,
        Err(err) => {
            eprintln!("phux: play: cannot read {}: {err}", args.file.display());
            return ExitCode::FAILURE;
        }
    };
    let loaded = match load(&file, args.idle_limit) {
        Ok(loaded) => loaded,
        Err(code) => return code,
    };

    let socket_path = args.socket.clone().unwrap_or_else(default_socket_path);
    if let Err(code) = crate::commands::ensure_socket_path_fits(&socket_path) {
        return code;
    }
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            eprintln!("phux: play: cannot resolve the phux binary to run in the pane: {err}");
            return ExitCode::FAILURE;
        }
    };

    let request_id = 1_u32;
    let frame = FrameKind::SpawnTerminal {
        request_id,
        // v0.1 servers expose the single default group (SPEC L1 §3.1).
        group: phux_protocol::ids::GroupId::new(1),
        command: Some(writer_argv(&exe, &file, &socket_path, args)),
        // No cwd: the writer reads one absolute file and writes to its own
        // stdout. It has nothing to resolve against a directory.
        cwd: None,
        env: None,
        term: None,
        // Placement is local-only (`dispatch_spawn_placed` refuses a
        // satellite owner), so a playback pane is never routed to one.
        satellite: None,
        owner_terminal: None,
    };

    // An omitted TARGET means `.`, the focused pane — so a human running
    // this while attached sees the recording appear beside what they are
    // looking at, rather than in whichever session the server last touched.
    // Placement is the whole reason the default is not "unplaced": a pane
    // outside the layout is a pane nobody watches.
    let target = args.target.unwrap_or(".");
    let spawned = match crate::commands::spawn::dispatch_spawn_placed(
        &socket_path,
        frame,
        request_id,
        "play",
        target,
        args.split,
        args.ratio,
    ) {
        Ok(result) => result,
        Err(code) => return code,
    };
    match spawned {
        SpawnResult::Ok(pane) => report(&pane, &file, &loaded, args),
        SpawnResult::Err(err) => {
            crate::commands::spawn::report_spawn_error(&err);
            ExitCode::FAILURE
        }
        other => {
            eprintln!("phux: unexpected SPAWN_TERMINAL result: {other:?}");
            ExitCode::FAILURE
        }
    }
}

/// Build the argv the pane runs: this binary, in writer mode.
///
/// Every option is passed explicitly, including `--socket` even when the
/// user did not give one. The pane's child inherits `PHUX_SOCKET` from the
/// server and would resolve the same path anyway, but "would anyway" is how
/// a playback pane ends up dialing the default socket from a server bound
/// somewhere else. The launcher already resolved the path; it says so.
fn writer_argv(exe: &Path, file: &Path, socket: &Path, spec: &PlayArgs<'_>) -> Vec<String> {
    let mut argv = vec![
        exe.to_string_lossy().into_owned(),
        "play".to_owned(),
        "--pty-writer".to_owned(),
        "--socket".to_owned(),
        socket.to_string_lossy().into_owned(),
        "--speed".to_owned(),
        spec.speed.get().to_string(),
    ];
    if let Some(limit) = spec.idle_limit {
        argv.push("--idle-limit".to_owned());
        argv.push(limit.to_string());
    }
    match spec.passes {
        // Absent: a single pass, the default on both sides. Nothing to say.
        Some(1) => {}
        // `--loop N`.
        Some(n) => {
            argv.push("--loop".to_owned());
            argv.push(n.to_string());
        }
        // Bare `--loop`: forever, which the CLI spells as 0.
        None => {
            argv.push("--loop".to_owned());
            argv.push("0".to_owned());
        }
    }
    if spec.no_fit {
        argv.push("--no-fit".to_owned());
    }
    if spec.close {
        argv.push("--close".to_owned());
    }
    argv.push(file.to_string_lossy().into_owned());
    argv
}

/// Report the pane that is now playing.
///
/// The id is the payload: everything a caller does next — attach, snapshot,
/// resize, kill — is addressed by it. The rest is what the caller cannot see
/// from outside, namely how long this will take at the speed they chose.
fn report(pane: &TerminalId, file: &Path, loaded: &Loaded, args: &PlayArgs<'_>) -> ExitCode {
    let length = pass_duration(&loaded.events, args.speed);
    let name = short_name(file);
    if args.json {
        let doc = serde_json::json!({
            "terminal_id": pane.local_id(),
            "path": file.display().to_string(),
            "cols": loaded.header.cols,
            "rows": loaded.header.rows,
            "events": loaded.events.len(),
            "speed": args.speed.get(),
            "idle_limit": loaded.idle_limit,
            "duration_ms": u64::try_from(length.as_millis()).unwrap_or(u64::MAX),
            "passes": args.passes,
        });
        outln!("{doc}");
        return ExitCode::SUCCESS;
    }
    let id = crate::selector::format_terminal_id(pane);
    let secs = length.as_secs_f64();
    let speed = args.speed.get();
    let events = loaded.events.len();
    outln!("playing {name} in terminal {id} ({events} events, {secs:.1}s at {speed}x)");
    ExitCode::SUCCESS
}

/// The file's basename, for a title or a one-liner.
fn short_name(file: &Path) -> String {
    file.file_name().map_or_else(
        || file.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

// ------------------------------------------------------------------ writer

/// The in-pane half: this process's stdout *is* the pane's PTY.
///
/// Diagnostics here go to the same PTY, which is correct and not a
/// compromise: this process has no other channel, and the pane is exactly
/// where a human or an agent will look for the reason a recording did not
/// play. It is also why the launcher validates first — by the time this runs
/// the only failures left are ones that appeared between the two.
fn run_writer(args: &PlayArgs<'_>) -> ExitCode {
    let loaded = match load(args.file, args.idle_limit) {
        Ok(loaded) => loaded,
        Err(code) => return code,
    };
    let socket_path = args.socket.clone().unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    let pane = own_pane();

    // Both of these are best-effort and both are about *not* corrupting the
    // playback: one stops the line discipline from rewriting the recorded
    // bytes, the other stops a stray keystroke from being echoed into the
    // middle of a frame.
    quiet_own_tty();

    if !args.no_fit {
        fit(&rt, &socket_path, pane.as_ref(), &loaded.header);
    }
    set_title(&format!("phux play {}", short_name(args.file)));

    let mut remaining = args.passes;
    loop {
        play_pass(&rt, &socket_path, pane.as_ref(), &loaded.events, args);
        match remaining {
            Some(n) if n <= 1 => break,
            Some(n) => {
                remaining = Some(n - 1);
            }
            None => {}
        }
        crate::output::bytes_now(LOOP_RESET);
    }

    // A pane that erased itself the instant the last byte landed would be
    // unobservable — the final frame is the artifact, and a caller that
    // snapshots it is racing the process exit. Holding is therefore the
    // default and `--close` is the opt-out; `phux kill @id` ends a held pane.
    set_title(&format!("phux play {} (ended)", short_name(args.file)));
    if args.close {
        return ExitCode::SUCCESS;
    }
    hold_forever();
}

/// Play the events once, on deadlines anchored to *this* pass's start.
///
/// Re-anchoring per pass rather than extrapolating from the first pass is
/// what keeps `--loop` honest on a loaded box: a pass that ran 200 ms late
/// hands the next pass a fresh clock instead of a debt it can never repay.
/// Within a pass nothing drifts, because every deadline comes from
/// [`due_at`] and not from the previous event (see that module's docs).
fn play_pass(
    rt: &tokio::runtime::Runtime,
    socket: &Path,
    pane: Option<&TerminalId>,
    events: &[CastEvent],
    args: &PlayArgs<'_>,
) {
    let anchor = Instant::now();
    for event in events {
        sleep_until(anchor + due_at(event.time_ms, args.speed));
        match event.code {
            EventCode::Output => crate::output::bytes_now(event.data.as_bytes()),
            // A recorded resize is a real event in the recording's life: the
            // bytes after it were painted at the new geometry and will wrap
            // wrong without it. `--no-fit` opts out of every size change,
            // this one included, because a caller that pinned the pane's
            // grid meant it.
            EventCode::Resize if !args.no_fit => {
                if let (Some(pane), Ok(geometry)) = (pane, parse_geometry(&event.data)) {
                    let _ = rt.block_on(phux_client::resize::resize_to(
                        socket,
                        pane,
                        geometry.cols,
                        geometry.rows,
                    ));
                }
            }
            // `i` is recorded *input*. Replaying it would type the
            // recording's keystrokes into a PTY whose reader is this
            // process, which no consumer expects and phux never records
            // anyway (ADR-0060 decision 5). `m` is a marker and `x` is the
            // recorded exit status; neither paints.
            EventCode::Resize | EventCode::Input | EventCode::Marker | EventCode::Exit => {}
        }
    }
}

/// Sleep until `deadline`, or return immediately if it has passed.
fn sleep_until(deadline: Instant) {
    let now = Instant::now();
    if deadline > now {
        std::thread::sleep(deadline - now);
    }
}

/// Resize the pane to the recording's own grid, and say so if it did not
/// take.
///
/// A cast is a stream of bytes that were correct for one geometry. Played
/// into a narrower pane, every line long enough to wrap does so in the wrong
/// place and every absolute cursor address lands somewhere else — the output
/// is not "slightly off", it is garbage. So the default is to make the pane
/// match the recording, using the frame `phux resize` already uses.
///
/// When that fails the honest move is to say so and play anyway: the caller
/// asked to see the recording, a wrapped recording is still legible, and the
/// alternative — refusing — would make `phux play` unusable in exactly the
/// case it is most useful, a pane inside a session someone is attached to
/// (whose viewport owns the size under every `window-size` policy but
/// `manual`).
fn fit(
    rt: &tokio::runtime::Runtime,
    socket: &Path,
    pane: Option<&TerminalId>,
    header: &CastHeader,
) {
    let (Some(pane), Some(cols), Some(rows)) = (
        pane,
        NonZeroU16::new(header.cols),
        NonZeroU16::new(header.rows),
    ) else {
        return;
    };
    let Ok(outcome) = rt.block_on(phux_client::resize::resize_to(socket, pane, cols, rows)) else {
        return;
    };
    if outcome.held() {
        return;
    }
    let (have_cols, have_rows) = outcome.applied;
    // One line, into the pane, before anything is painted — most recordings
    // open by clearing the screen, so this would be invisible if it went
    // after the first frame.
    //
    // Written with an explicit CRLF rather than through `outln!`, because
    // `quiet_own_tty` has already cleared `OPOST` by the time this runs: a
    // bare `\n` would move down a row and leave the cursor in column 68,
    // which is precisely where the recording is about to start painting.
    crate::output::bytes_now(
        format!(
            "phux play: this pane is {have_cols}x{have_rows} but the recording \
             is {}x{} - output will wrap. An attached client's viewport owns \
             this pane's size; `window-size = \"manual\"` makes explicit sizes \
             stick.\r\n",
            header.cols, header.rows
        )
        .as_bytes(),
    );
}

/// The pane this process is running in, from the environment the server
/// injects into every spawned Terminal.
///
/// `PHUX_TERMINAL_ID` names *which pane* and `PHUX_SOCKET` (already resolved
/// into `--socket` by the launcher) names *which server*; only the pair
/// identifies a Terminal. `None` — someone ran the hidden writer mode by
/// hand outside a pane — degrades to "play the bytes, resize nothing", which
/// is the most this process can honestly do.
fn own_pane() -> Option<TerminalId> {
    let raw = std::env::var("PHUX_TERMINAL_ID").ok()?;
    raw.parse::<u32>().ok().map(TerminalId::local)
}

/// Stop the tty's line discipline from editing the recording.
///
/// Two flags, two distinct corruptions:
///
/// * `OPOST` (specifically `ONLCR`) rewrites every `\n` this process writes
///   into `\r\n`. The bytes in a cast came off a PTY *master*, i.e. they
///   have already been through one terminal's `OPOST`; sending them through
///   a second one applies it twice. For the common `\r\n` that is harmless,
///   but a recording that deliberately emitted a bare line feed — a
///   full-screen program advancing a row without returning the carriage —
///   would have the column silently reset under it. Playback is supposed to
///   deliver the recorded bytes, not a translation of them.
/// * `ECHO` means a keystroke aimed at this pane by a human or by
///   `phux send-keys` gets painted into the middle of a frame by the kernel.
///   Nothing here reads stdin, so an echo can only ever be corruption.
///
/// `ISIG` is deliberately left alone: Ctrl-C in a playback pane should stop
/// the playback, and with the signal disposition intact it does, for free.
///
/// # Why this only ever touches a tty this process *is*
///
/// Nothing here restores the old settings, and that is safe for exactly one
/// tty: the PTY of a pane whose process is this one. It dies when this
/// process does, so there is no "after" to restore for — and there could not
/// be a reliable restore anyway, since the writer's normal end is a park that
/// only a signal interrupts and a signal runs no destructor.
///
/// The guard is therefore `tcgetsid(stdout) == getpid()`: the server spawns a
/// pane's process into its own session with the PTY as its controlling
/// terminal, so the pane's process — and only it — is that session's leader.
/// A `phux play --pty-writer` a curious user runs by hand from a shell (in a
/// phux pane or not) is a *child* of the session leader, fails the test, and
/// leaves the terminal alone. Getting this wrong in the permissive direction
/// would leave that user's shell with no echo and no newline translation
/// after the command exited, which is a far worse bug than the double
/// `ONLCR` this avoids.
fn quiet_own_tty() {
    use std::io::IsTerminal as _;
    use std::os::fd::AsFd as _;

    let stdout = std::io::stdout();
    if !stdout.is_terminal() {
        return;
    }
    let fd = stdout.as_fd();
    if rustix::termios::tcgetsid(fd) != Ok(rustix::process::getpid()) {
        return;
    }
    let Ok(mut termios) = rustix::termios::tcgetattr(fd) else {
        return;
    };
    termios
        .output_modes
        .remove(rustix::termios::OutputModes::OPOST);
    termios
        .local_modes
        .remove(rustix::termios::LocalModes::ECHO);
    let _ = rustix::termios::tcsetattr(fd, rustix::termios::OptionalActions::Now, &termios);
}

/// Set the pane's terminal title (OSC 2), which the server already parses
/// and republishes as a `title_changed` event (`phux watch`).
///
/// This is how "the recording finished" becomes visible from outside without
/// painting a cell: the final frame stays exactly as the cast left it, and
/// the fact that it is final lives in metadata instead. Verified rather than
/// assumed — `phux watch @N --json` on a playback pane emits
/// `{"event":"title_changed","title":"phux play demo.cast (ended)"}`.
///
/// A cast that sets its own title during playback simply wins for the
/// duration, and the closing title is written after the last event either
/// way.
fn set_title(title: &str) {
    // BEL-terminated rather than ST: universally understood, and it is what
    // the shell integrations in the wild emit.
    crate::output::bytes_now(format!("\x1b]2;{title}\x07").as_bytes());
}

/// Keep the pane alive, holding the last frame, until something kills it.
fn hold_forever() -> ! {
    loop {
        std::thread::sleep(HOLD_TICK);
    }
}

// ------------------------------------------------------------------ shared

/// Read a cast and apply the one transformation both halves must agree on.
///
/// Called on both sides — the launcher to validate and to report the
/// duration, the writer to play it — with identical inputs, so the length
/// the user is told and the length they wait through are the same number by
/// construction rather than by two implementations agreeing.
fn load(file: &Path, idle_flag: Option<f64>) -> Result<Loaded, ExitCode> {
    let handle = std::fs::File::open(file).map_err(|err| {
        eprintln!("phux: play: cannot read {}: {err}", file.display());
        ExitCode::FAILURE
    })?;
    let (header, mut events) = read_cast(BufReader::new(handle)).map_err(|err| {
        eprintln!("phux: play: {}: {err}", file.display());
        ExitCode::FAILURE
    })?;
    let idle_limit = effective_idle_limit(&header, idle_flag);
    clamp_idle(&mut events, idle_limit);
    Ok(Loaded {
        header,
        events,
        idle_limit,
    })
}

/// Resolve the idle clamp: the flag if given, else the recording's own
/// `idle_time_limit` header field, else none.
///
/// Deferring to the header is what makes playback agree with the recorder
/// that produced the file: `phux rec` writes the limit it clamped with, and
/// asciinema's own player has honored that field since v2. Overriding it
/// unconditionally with a phux default would silently re-clamp a file that
/// was already clamped, and a caller who wants the raw timeline could never
/// get it back.
///
/// `0` (or any non-positive value) means "no clamp" on both surfaces,
/// matching `phux rec --idle-limit 0`.
fn effective_idle_limit(header: &CastHeader, flag: Option<f64>) -> Option<f64> {
    flag.or(header.idle_time_limit)
        .filter(|limit| limit.is_finite() && *limit > 0.0)
}

/// Parse `--speed` for clap's `value_parser`.
///
/// The bounds live in [`Speed`], next to the arithmetic that needs them; the
/// wording lives here, next to the user.
pub(crate) fn parse_speed(value: &str) -> Result<Speed, String> {
    let raw: f64 = value
        .parse()
        .map_err(|_| format!("speed must be a number, got '{value}'"))?;
    Speed::new(raw).ok_or_else(|| {
        format!(
            "speed must be between {} and {} (1 is real time), got '{value}'",
            Speed::MIN.get(),
            Speed::MAX.get()
        )
    })
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::unwrap_used,
        clippy::panic,
        reason = "tests"
    )]

    use super::*;

    fn header(idle: Option<f64>) -> CastHeader {
        CastHeader {
            cols: 80,
            rows: 24,
            timestamp: None,
            idle_time_limit: idle,
            command: None,
            title: None,
            env: std::collections::BTreeMap::new(),
            theme: None,
        }
    }

    fn args(file: &Path, passes: Option<u32>, idle: Option<f64>) -> PlayArgs<'_> {
        PlayArgs {
            file,
            target: None,
            speed: Speed::NORMAL,
            idle_limit: idle,
            passes,
            split: SpawnSplit::Horizontal,
            ratio: 0.5,
            no_fit: false,
            close: false,
            json: false,
            socket: None,
            pty_writer: false,
        }
    }

    #[test]
    fn parse_speed_accepts_the_documented_range_and_names_it_on_failure() {
        assert_eq!(parse_speed("1").expect("1 parses"), Speed::NORMAL);
        let parsed = parse_speed("2.5").expect("2.5 parses").get();
        assert!((parsed - 2.5).abs() < f64::EPSILON, "got {parsed}");
        let err = parse_speed("0").expect_err("zero must be rejected");
        assert!(
            err.contains("0.01"),
            "diagnostic must name the bound: {err}"
        );
        assert!(parse_speed("fast").is_err());
        assert!(parse_speed("-1").is_err());
    }

    #[test]
    fn the_flag_beats_the_header_and_zero_means_no_clamp() {
        // The recording says 2s; the caller says 0.5s. The caller wins.
        assert_eq!(
            effective_idle_limit(&header(Some(2.0)), Some(0.5)),
            Some(0.5)
        );
        // No flag: the recording's own limit is honored, so playback agrees
        // with the recorder that wrote the file.
        assert_eq!(effective_idle_limit(&header(Some(2.0)), None), Some(2.0));
        // Neither: the raw timeline.
        assert_eq!(effective_idle_limit(&header(None), None), None);
        // An explicit 0 disables a header limit rather than clamping to zero
        // — the same spelling `phux rec --idle-limit 0` uses.
        assert_eq!(effective_idle_limit(&header(Some(2.0)), Some(0.0)), None);
        assert_eq!(effective_idle_limit(&header(Some(f64::NAN)), None), None);
    }

    #[test]
    fn writer_argv_round_trips_through_the_cli_parser() {
        // The launcher and the writer are the same binary, so the argv one
        // builds MUST parse in the other. Asserting it here is what keeps a
        // renamed flag from turning every playback pane into an instant
        // clap usage error that only an e2e would catch.
        use clap::Parser as _;

        let file = Path::new("/tmp/demo.cast");
        let mut spec = args(file, Some(3), Some(1.5));
        spec.no_fit = true;
        spec.close = true;
        let argv = writer_argv(
            Path::new("/usr/local/bin/phux"),
            file,
            Path::new("/tmp/phux.sock"),
            &spec,
        );

        let cli = crate::Cli::try_parse_from(&argv).expect("the writer argv must parse");
        // `--socket` is the root global now, so the writer's copy lands on
        // the top-level field rather than inside the Play variant.
        assert_eq!(cli.socket.as_deref(), Some(Path::new("/tmp/phux.sock")));
        let Some(crate::Command::Play {
            file: parsed_file,
            speed,
            idle_limit,
            loops,
            no_fit,
            close,
            pty_writer,
            ..
        }) = cli.command
        else {
            panic!("expected the play subcommand");
        };
        assert!(pty_writer, "the pane's process must be in writer mode");
        assert_eq!(parsed_file, file);
        assert_eq!(speed, Speed::NORMAL);
        assert_eq!(idle_limit, Some(1.5));
        assert_eq!(loops, Some(3));
        assert!(no_fit);
        assert!(close);
    }

    #[test]
    fn writer_argv_spells_forever_as_the_bare_loop_flag() {
        use clap::Parser as _;

        let file = Path::new("/tmp/demo.cast");
        // `None` passes = play until killed. It must survive the round trip
        // as `Some(0)`, the CLI's spelling of forever — an argv that dropped
        // it would silently turn an infinite loop into a single pass.
        let argv = writer_argv(
            Path::new("/usr/local/bin/phux"),
            file,
            Path::new("/tmp/phux.sock"),
            &args(file, None, None),
        );
        let cli = crate::Cli::try_parse_from(&argv).expect("the writer argv must parse");
        let Some(crate::Command::Play { loops, .. }) = cli.command else {
            panic!("expected the play subcommand");
        };
        assert_eq!(loops, Some(0));

        // A single pass is the default on both sides, so it says nothing.
        let single = writer_argv(
            Path::new("/usr/local/bin/phux"),
            file,
            Path::new("/tmp/phux.sock"),
            &args(file, Some(1), None),
        );
        assert!(
            !single.iter().any(|arg| arg == "--loop"),
            "a single pass must not emit --loop: {single:?}"
        );
    }

    #[test]
    fn writer_argv_always_names_the_socket() {
        // Even with no `--socket` from the user: the launcher resolved a
        // path and the pane must dial that one, not re-derive a default
        // that may differ from the server it was spawned by.
        let file = Path::new("/tmp/demo.cast");
        let argv = writer_argv(
            Path::new("/usr/local/bin/phux"),
            file,
            Path::new("/run/user/1000/phux/phux.sock"),
            &args(file, Some(1), None),
        );
        let socket_at = argv
            .iter()
            .position(|arg| arg == "--socket")
            .expect("argv must carry --socket");
        assert_eq!(argv[socket_at + 1], "/run/user/1000/phux/phux.sock");
        assert_eq!(argv[0], "/usr/local/bin/phux", "argv[0] is the phux binary");
        assert_eq!(
            argv.last().map(String::as_str),
            Some("/tmp/demo.cast"),
            "the cast is the trailing positional"
        );
    }

    #[test]
    fn load_rejects_a_file_that_is_not_a_cast() {
        let dir = std::env::temp_dir().join(format!("phux-play-load-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("not-a-cast.txt");
        std::fs::write(&path, b"this is not asciicast\n").expect("write");
        assert!(
            load(&path, None).is_err(),
            "a non-cast must fail in the launcher, not in the pane"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_clamps_with_the_headers_own_limit() {
        let dir = std::env::temp_dir().join(format!("phux-play-clamp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("gap.cast");
        std::fs::write(
            &path,
            b"{\"version\":2,\"width\":80,\"height\":24,\"idle_time_limit\":2.0}\n\
              [0.0, \"o\", \"a\"]\n\
              [30.0, \"o\", \"b\"]\n",
        )
        .expect("write");

        let loaded = load(&path, None).expect("a well-formed cast loads");
        assert_eq!(loaded.idle_limit, Some(2.0));
        assert_eq!(
            loaded.events[1].time_ms, 2_000,
            "a 30s pause under a 2s header limit must collapse to 2s"
        );

        // And the flag overrides it, all the way through to the timeline.
        let loaded = load(&path, Some(0.25)).expect("cast loads");
        assert_eq!(loaded.events[1].time_ms, 250);
        std::fs::remove_dir_all(&dir).ok();
    }
}
