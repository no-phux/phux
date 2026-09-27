//! `phux play` — replay a recording *as a pane* (ADR-0064): an ordinary
//! Terminal whose PTY is fed from the cast, so it can be attached, snapshot,
//! resized, observed, and killed like any pane.
//!
//! [`run_play`] has two modes. The launcher validates the cast and
//! `SPAWN_RESOURCE`s a pane whose command is this same binary with the hidden
//! `--pty-writer`; TARGET says where the pane goes, never what is overwritten.
//! The writer runs inside that pane, where stdout is the PTY. Both halves are
//! the same executable ([`std::env::current_exe`]), so the joining argv is not
//! a compatibility surface. No wire change was needed.

use std::io::BufReader;
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{FrameKind, SpawnResult};
use phux_record::cast::{CastEvent, CastHeader, EventCode, read_cast};
use phux_record::playback::{Speed, due_at, pass_duration};
use phux_record::timeline::clamp_idle;
use phux_server::runtime::default_socket_path;

use crate::commands::{SpawnSplit, cli_runtime, resize::parse_geometry};

/// Written between loop passes: DECSTR, leave the alternate screen, erase
/// screen and scrollback, home. Not `RIS`, which would also reset the grid size
/// the pane was fitted to; `?1049l` because DECSTR does not leave the alt
/// screen a `vim` recording ends in.
const LOOP_RESET: &[u8] = b"\x1b[!p\x1b[?1049l\x1b[2J\x1b[3J\x1b[H";

/// How long the writer sleeps between checks while holding the final frame;
/// long, since there is nothing to check.
const HOLD_TICK: Duration = Duration::from_secs(3600);

/// Everything `phux play` was asked to do.
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

/// The user-facing half: validate in the caller's terminal (so a bad file is
/// a plain stderr line), spawn a pane that plays, report it.
fn run_launcher(args: &PlayArgs<'_>) -> ExitCode {
    // Absolute: the pane's child is spawned by the daemon, not from this cwd.
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
    let frame = FrameKind::SpawnResource {
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
        agent_session: None,
        initial_size: None,
        resource: None,
    };

    // An omitted TARGET means `.`, so an attached human sees the recording
    // appear beside what they are looking at.
    let target = args.target.unwrap_or(".");
    let spawned = match crate::commands::spawn::dispatch_spawn_placed(
        &socket_path,
        frame,
        request_id,
        "play",
        target,
        args.split,
        args.ratio,
        None,
        None,
        args.json,
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
        // `SpawnResult` is `#[non_exhaustive]`: a kind with no arm here is a
        // vocabulary this client does not have, i.e. version skew.
        _ => {
            eprintln!(
                "phux: {}",
                phux_client::explain::unexpected_reply("SPAWN_RESOURCE")
            );
            ExitCode::FAILURE
        }
    }
}

/// Build the writer argv: this binary in writer mode, with every option
/// explicit, including the already-resolved `--socket`.
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

/// Report the pane that is now playing: its id, and the duration at the
/// chosen speed.
fn report(pane: &ResourceId, file: &Path, loaded: &Loaded, args: &PlayArgs<'_>) -> ExitCode {
    let length = pass_duration(&loaded.events, args.speed);
    let name = short_name(file);
    if args.json {
        let duration_ms = u64::try_from(length.as_millis()).unwrap_or(u64::MAX);
        outln!("{}", play_json(pane, file, loaded, args, duration_ms));
        return ExitCode::SUCCESS;
    }
    let id = crate::selector::format_terminal_id(pane);
    let secs = length.as_secs_f64();
    let speed = args.speed.get();
    let events = loaded.events.len();
    outln!("playing {name} in terminal {id} ({events} events, {secs:.1}s at {speed}x)");
    ExitCode::SUCCESS
}

/// The `phux play --json` result document. Pure, so the shape (including
/// `schema_version`) is unit-testable without spawning a pane.
fn play_json(
    pane: &ResourceId,
    file: &Path,
    loaded: &Loaded,
    args: &PlayArgs<'_>,
    duration_ms: u64,
) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "terminal_id": pane.local_id(),
        "path": file.display().to_string(),
        "cols": loaded.header.cols,
        "rows": loaded.header.rows,
        "events": loaded.events.len(),
        "speed": args.speed.get(),
        "idle_limit": loaded.idle_limit,
        "duration_ms": duration_ms,
        "passes": args.passes,
    })
}

/// The file's basename, for a title or a one-liner.
fn short_name(file: &Path) -> String {
    file.file_name().map_or_else(
        || file.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

// ------------------------------------------------------------------ writer

/// The in-pane half: stdout is the pane's PTY, which is also where its
/// diagnostics belong. The launcher already validated.
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

    // Best-effort: keep the line discipline from rewriting recorded bytes and
    // from echoing stray keystrokes into a frame.
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

    // Hold the final frame by default (it is the artifact); `--close` opts out
    // and `phux kill @id` ends a held pane.
    set_title(&format!("phux play {} (ended)", short_name(args.file)));
    if args.close {
        return ExitCode::SUCCESS;
    }
    hold_forever();
}

/// Play the events once, with deadlines anchored to this pass's start, so a
/// late pass under load does not push its debt into the next.
fn play_pass(
    rt: &tokio::runtime::Runtime,
    socket: &Path,
    pane: Option<&ResourceId>,
    events: &[CastEvent],
    args: &PlayArgs<'_>,
) {
    let anchor = Instant::now();
    for event in events {
        sleep_until(anchor + due_at(event.time_ms, args.speed));
        match event.code {
            EventCode::Output => crate::output::bytes_now(event.data.as_bytes()),
            // A recorded resize is replayed (later bytes were painted at that size)
            // unless `--no-fit` pinned the grid.
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
            // `i` (input) is never replayed into the PTY; `m` and `x` do not paint.
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

/// Resize the pane to the recording's grid (a cast played into the wrong
/// size is garbage), and if that does not take, say so and play anyway: a
/// viewer's viewport may own the size.
fn fit(
    rt: &tokio::runtime::Runtime,
    socket: &Path,
    pane: Option<&ResourceId>,
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
    // One line into the pane before anything paints (recordings usually clear
    // first), with an explicit CRLF because `OPOST` is already off.
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

/// The pane this process runs in, from the injected `PHUX_TERMINAL_ID`.
/// `None` outside a pane degrades to "play the bytes, resize nothing".
fn own_pane() -> Option<ResourceId> {
    let raw = std::env::var("PHUX_TERMINAL_ID").ok()?;
    raw.parse::<u32>().ok().map(ResourceId::local)
}

/// Stop the tty's line discipline from editing the recording: clear `OPOST`
/// (a cast's bytes already went through one `ONLCR`) and `ECHO` (nothing reads
/// stdin). `ISIG` stays, so Ctrl-C stops playback.
///
/// Nothing is restored, so this only touches a tty this process owns: the guard
/// is `tcgetsid(stdout) == getpid()`, true only for a pane's session leader. A
/// writer run by hand from a shell fails it and leaves that shell's terminal
/// alone.
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

/// Set the pane title (OSC 2), which the server republishes as
/// `title_changed`: how "the recording finished" is visible without painting a
/// cell.
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

/// Read a cast and apply the idle clamp. Both halves call this with the same
/// inputs, so the reported and the played durations agree by construction.
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

/// The idle clamp: the flag, else the cast header's `idle_time_limit` (what
/// `phux rec` clamped with), else none. `0` or less means no clamp.
fn effective_idle_limit(header: &CastHeader, flag: Option<f64>) -> Option<f64> {
    flag.or(header.idle_time_limit)
        .filter(|limit| limit.is_finite() && *limit > 0.0)
}

/// Parse `--speed` for clap; the bounds live in [`Speed`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct SpeedArg(pub Speed);

impl std::str::FromStr for SpeedArg {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        parse_speed(value).map(Self)
    }
}

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

    /// `phux play --json` pins `schema_version` 1 plus the documented result
    /// fields (§4.16).
    #[test]
    fn play_json_pins_the_contract_shape() {
        let file = Path::new("/home/me/demo.cast");
        let loaded = Loaded {
            header: header(Some(2.0)),
            events: vec![CastEvent {
                time_ms: 0,
                code: EventCode::Output,
                data: "hi".to_owned(),
            }],
            idle_limit: Some(2.0),
        };
        let spec = args(file, Some(1), Some(2.0));
        let pane = ResourceId::local(7);
        let doc = play_json(&pane, file, &loaded, &spec, 17_198);
        assert_eq!(doc["schema_version"], 1);
        assert_eq!(doc["terminal_id"], 7);
        assert_eq!(doc["path"], "/home/me/demo.cast");
        assert_eq!(doc["cols"], 80);
        assert_eq!(doc["rows"], 24);
        assert_eq!(doc["events"], 1);
        assert_eq!(doc["speed"], 1.0);
        assert_eq!(doc["idle_limit"], 2.0);
        assert_eq!(doc["duration_ms"], 17_198);
        assert_eq!(doc["passes"], 1);
        assert_eq!(doc.as_object().map(serde_json::Map::len), Some(10));
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
        // The writer argv the launcher builds must parse in the same binary.

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

        let cli = crate::parse_cli(&argv).expect("the writer argv must parse");
        // `--socket` is the root global now, so the writer's copy lands on
        // the top-level field rather than inside the Play variant.
        assert_eq!(cli.socket.as_deref(), Some(Path::new("/tmp/phux.sock")));
        let Some(crate::commands::Command::Play {
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
        assert_eq!(speed.0, Speed::NORMAL);
        assert_eq!(idle_limit, Some(1.5));
        assert_eq!(loops, Some(3));
        assert!(no_fit);
        assert!(close);
    }

    #[test]
    fn writer_argv_spells_forever_as_the_bare_loop_flag() {
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
        let cli = crate::parse_cli(&argv).expect("the writer argv must parse");
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
