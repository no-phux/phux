//! `phux play` — replay a recording *as a pane* (ADR-0064): an ordinary
//! Terminal whose PTY is fed from the cast, so it can be attached, snapshot,
//! resized, observed, and killed like any pane.
//!
//! [`run`] has two modes. The launcher validates the cast and
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

use phux_config::socket::default_socket_path;
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{FrameKind, SpawnResult};
use phux_record::cast::{CastEvent, CastHeader, EventCode, read_cast};
use phux_record::playback::{Speed, due_at, pass_duration};
use phux_record::timeline::clamp_idle;

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
pub(crate) struct Request<'a> {
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

/// Run `phux play` as parsed.
pub(crate) fn run(args: crate::commands::PlayArgs, socket: Option<PathBuf>) -> ExitCode {
    let crate::commands::PlayArgs {
        file,
        target,
        speed,
        idle_limit,
        loops,
        split,
        ratio,
        no_fit,
        close,
        json,
        pty_writer,
    } = args;
    run_play(&Request {
        file: &file,
        target: target.as_deref(),
        speed: speed.0,
        idle_limit,
        // The CLI spells "repeat forever" as `--loop` with no value,
        // which clap fills in as 0; `passes` carries that as `None` so
        // the player's loop condition is a plain "count remaining".
        passes: match loops {
            None => Some(1),
            Some(0) => None,
            Some(n) => Some(n),
        },
        split,
        ratio,
        no_fit,
        close,
        json: json.json,
        socket,
        pty_writer,
    })
}

/// Run `phux play` in whichever of its two modes the argv selected.
fn run_play(args: &Request<'_>) -> ExitCode {
    if args.pty_writer {
        return run_writer(args);
    }
    run_launcher(args)
}

// ---------------------------------------------------------------- launcher

/// The user-facing half: validate in the caller's terminal (so a bad file is
/// a plain stderr line), spawn a pane that plays, report it.
fn run_launcher(args: &Request<'_>) -> ExitCode {
    // Absolute: the pane's child is spawned by the daemon, not from this cwd.
    let file = match std::fs::canonicalize(args.file) {
        Ok(path) => path,
        Err(err) => {
            error!("play: cannot read {}: {err}", args.file.display());
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
            error!("play: cannot resolve the phux binary to run in the pane: {err}");
            return ExitCode::FAILURE;
        }
    };

    let request_id = 1_u32;
    let frame = FrameKind::SpawnResource {
        request_id,
        // v0.1 servers expose the single default group.
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
        SpawnResult::Err(err) => crate::commands::spawn::report_spawn_error(args.json, &err),
        // `SpawnResult` is `#[non_exhaustive]`: a kind with no arm here is a
        // vocabulary this client does not have, i.e. version skew.
        _ => {
            error!(
                "{}",
                phux_client::explain::unexpected_reply("SPAWN_RESOURCE")
            );
            ExitCode::FAILURE
        }
    }
}

/// Build the writer argv: this binary in writer mode, with every option
/// explicit, including the already-resolved `--socket`.
fn writer_argv(exe: &Path, file: &Path, socket: &Path, spec: &Request<'_>) -> Vec<String> {
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
fn report(pane: &ResourceId, file: &Path, loaded: &Loaded, args: &Request<'_>) -> ExitCode {
    let length = pass_duration(&loaded.events, args.speed);
    let name = short_name(file);
    if args.json {
        let duration_ms = u64::try_from(length.as_millis()).unwrap_or(u64::MAX);
        outln!("{}", play_json(pane, file, loaded, args, duration_ms));
        return ExitCode::SUCCESS;
    }
    let id = phux_client::selector::format_terminal_id(pane);
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
    args: &Request<'_>,
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
fn run_writer(args: &Request<'_>) -> ExitCode {
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
    args: &Request<'_>,
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
    crate::environment::own_terminal_id().map(ResourceId::local)
}

/// Stop the tty's line discipline from editing the recording: clear `OPOST`
/// (a cast's bytes already went through one `ONLCR`) and `ECHO` (nothing reads
/// stdin). `ISIG` stays, so Ctrl-C stops playback.
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
        error!("play: cannot read {}: {err}", file.display());
        ExitCode::FAILURE
    })?;
    let (header, mut events) = read_cast(BufReader::new(handle)).map_err(|err| {
        error!("play: {}: {err}", file.display());
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
mod tests;
