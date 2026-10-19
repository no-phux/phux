//! `phux perf --desktop`: the same telemetry, read from a running phux
//! desktop's dev socket instead of the server.
//!
//! A desktop in dev mode (`PHUX_DEV=1` or `--dev`) listens on
//! `desktop-<pid>.dev.sock` beside its server's socket and answers one line
//! of JSON per request (`clients/desktop/native/src/devtools/socket.rs`).
//! This finds the live one for the socket in use, asks, and prints what it
//! says: the report as a table or JSON, a merged desktop-and-server trace, or
//! a HUD switch.

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use phux_perf::PerfReport;
use serde_json::{Value, json};

use crate::commands::json_err::Refusal;
use crate::commands::perf::{AllocSwitch, MIN_WATCH, PerfOptions};

/// Environment override for the desktop's dev socket, the same variable the
/// desktop reads.
const PATH_ENV: &str = "PHUX_DESKTOP_DEV_SOCKET";

/// How long a trace may take beyond its recording before the desktop is
/// presumed stuck: it also waits for the server and writes two files.
const TRACE_SLACK: Duration = Duration::from_secs(30);

/// What to ask the desktop.
#[derive(Debug)]
pub(crate) enum Ask {
    Report(PerfOptions),
    Trace { seconds: f64, out: Option<PathBuf> },
    Alloc(AllocSwitch),
    Hud(AllocSwitch),
}

/// Run `ask` against the desktop serving `server_socket`.
pub(crate) fn run(json: bool, ask: Ask, server_socket: &Path) -> ExitCode {
    match find(server_socket).and_then(|path| dispatch(json, ask, &path)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(refusal) => refusal.emit(json),
    }
}

fn dispatch(json: bool, ask: Ask, path: &Path) -> Result<(), Refusal> {
    match ask {
        Ask::Report(opts) => report(&opts, path),
        Ask::Trace { seconds, out } => trace(json, seconds, out.as_deref(), path),
        Ask::Alloc(state) => switch(json, "alloc", state, path),
        Ask::Hud(state) => switch(json, "hud", state, path),
    }
}

/// The live dev socket: [`PATH_ENV`], else the newest `desktop-*.dev.sock`
/// beside `server_socket` that accepts a connection.
fn find(server_socket: &Path) -> Result<PathBuf, Refusal> {
    if let Some(path) = std::env::var_os(PATH_ENV) {
        return Ok(PathBuf::from(path));
    }
    let dir = server_socket.parent().unwrap_or_else(|| Path::new("."));
    let mut live: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| is_dev_socket(path))
        .filter(|path| live_or_prune(path))
        .map(|path| {
            let modified = std::fs::metadata(&path)
                .and_then(|meta| meta.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            (modified, path)
        })
        .collect();
    live.sort();
    live.pop().map(|(_, path)| path).ok_or_else(|| {
        Refusal::new(
            "no_desktop",
            format!("no phux desktop in dev mode is serving {}", dir.display()),
            "start the desktop with PHUX_DEV=1 (or `phux-desktop --dev`) against this socket, \
             or point PHUX_DESKTOP_DEV_SOCKET at its dev socket",
            1,
        )
    })
}

/// Whether a desktop answers at `path`. A socket nobody listens on belongs
/// to a desktop that was killed; it is removed so it is not probed again.
fn live_or_prune(path: &Path) -> bool {
    match UnixStream::connect(path) {
        Ok(_) => true,
        Err(error) => {
            if error.kind() == std::io::ErrorKind::ConnectionRefused {
                let _ = std::fs::remove_file(path);
            }
            false
        }
    }
}

fn is_dev_socket(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("desktop-") && name.ends_with(".dev.sock"))
}

/// One request, one reply; a reply with `ok: false` is a refusal.
fn ask(path: &Path, request: &Value, timeout: Duration) -> Result<Value, Refusal> {
    let unreachable = |error: std::io::Error| {
        Refusal::new(
            "desktop_unreachable",
            format!(
                "the desktop dev socket {} did not answer: {error}",
                path.display()
            ),
            "check the desktop is still running in dev mode",
            1,
        )
    };
    let mut stream = UnixStream::connect(path).map_err(unreachable)?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(unreachable)?;
    writeln!(stream, "{request}").map_err(unreachable)?;
    let mut line = String::new();
    BufReader::new(&stream)
        .read_line(&mut line)
        .map_err(unreachable)?;
    let reply: Value = serde_json::from_str(&line).map_err(|error| {
        Refusal::new(
            "desktop_protocol",
            format!("the desktop answered something that is not JSON: {error}"),
            "check the desktop and this CLI come from the same release",
            1,
        )
    })?;
    if reply["ok"] == json!(false) {
        return Err(Refusal::new(
            "desktop_refused",
            reply["error"].as_str().unwrap_or("refused").to_owned(),
            "",
            1,
        ));
    }
    Ok(reply)
}

fn report(opts: &PerfOptions, path: &Path) -> Result<(), Refusal> {
    let read = || -> Result<(PerfReport, Value), Refusal> {
        let reply = ask(path, &json!({ "op": "perf" }), Duration::from_secs(5))?;
        let report = serde_json::from_value(reply["report"].clone()).map_err(|error| {
            Refusal::new(
                "desktop_protocol",
                format!("the desktop's report did not parse: {error}"),
                "check the desktop and this CLI come from the same release",
                1,
            )
        })?;
        Ok((report, reply["frames"].clone()))
    };
    if opts.reset {
        ask(path, &json!({ "op": "reset" }), Duration::from_secs(5))?;
    }
    let Some(every) = opts.watch else {
        let (report, frames) = read()?;
        print(opts.json, &report, &frames, None);
        return Ok(());
    };
    let every = Duration::try_from_secs_f64(every)
        .map_err(|_| {
            Refusal::new(
                "invalid_argument",
                format!("--watch {every} is not a usable interval"),
                "give a finite number of seconds",
                2,
            )
        })?
        .max(MIN_WATCH);
    let (mut prev, _) = read()?;
    loop {
        std::thread::sleep(every);
        let (cur, frames) = read()?;
        if !opts.json {
            out!("\x1b[H\x1b[2J");
        }
        print(opts.json, &cur.delta(&prev), &frames, Some(every));
        prev = cur;
    }
}

fn print(json: bool, report: &PerfReport, frames: &Value, interval: Option<Duration>) {
    if json {
        outln!("{}", json!({ "report": report, "frames": frames }));
        return;
    }
    outln!(
        "frames  {} fps  build p50 {}us p99 {}us max {}us  over budget {}  jank {}  worst gap {}us",
        frames["fps"]
            .as_f64()
            .map_or_else(|| "-".to_owned(), |fps| format!("{fps:.0}")),
        frames["build_p50_us"],
        frames["build_p99_us"],
        frames["build_max_us"],
        frames["over_budget"],
        frames["janks"],
        frames["worst_gap_us"],
    );
    out!("{}", phux_perf::render_report(report, interval));
}

fn trace(json: bool, seconds: f64, out: Option<&Path>, path: &Path) -> Result<(), Refusal> {
    let span = Duration::try_from_secs_f64(seconds).map_err(|_| {
        Refusal::new(
            "invalid_argument",
            format!("--seconds {seconds} is not a usable duration"),
            "give a positive number of seconds",
            2,
        )
    })?;
    if !json {
        info!("recording a desktop and server trace for {seconds}s");
    }
    let mut reply = ask(
        path,
        &json!({ "op": "trace", "seconds": seconds, "server": true }),
        span + TRACE_SLACK,
    )?;
    let written = reply["path"].as_str().unwrap_or_default().to_owned();
    if let Some(dest) = out {
        std::fs::copy(&written, dest).map_err(|error| {
            Refusal::new(
                "io",
                format!(
                    "cannot copy the trace {written} to {}: {error}",
                    dest.display()
                ),
                "",
                1,
            )
        })?;
        reply["out"] = json!(dest.display().to_string());
    }
    if json {
        outln!("{reply}");
        return Ok(());
    }
    outln!("{}", reply["out"].as_str().unwrap_or(&written));
    if let Some(error) = reply["server_error"].as_str() {
        warn!("the server did not take part: {error}");
    }
    info!(
        "{} desktop events, {} dropped; open it in https://ui.perfetto.dev",
        reply["events"], reply["dropped"]
    );
    Ok(())
}

fn switch(json: bool, op: &str, state: AllocSwitch, path: &Path) -> Result<(), Refusal> {
    let on = state == AllocSwitch::On;
    let reply = ask(path, &json!({ "op": op, "on": on }), Duration::from_secs(5))?;
    if json {
        outln!("{reply}");
    } else {
        let what = if op == "hud" {
            "HUD"
        } else {
            "heap accounting"
        };
        outln!("desktop {what} {}", if on { "on" } else { "off" });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixListener;

    use super::*;

    #[test]
    fn dev_socket_names() {
        assert!(is_dev_socket(Path::new("/r/desktop-42.dev.sock")));
        assert!(!is_dev_socket(Path::new("/r/default.sock")));
        assert!(!is_dev_socket(Path::new("/r/desktop-42.sock")));
    }

    #[test]
    fn finds_only_a_live_desktop_and_reads_its_reply() {
        let dir = std::env::temp_dir().join(format!("phux-perf-desktop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
        // A stale socket file nobody listens on is skipped.
        drop(UnixListener::bind(dir.join("desktop-1.dev.sock")).unwrap_or_else(|e| panic!("{e}")));
        let serving = dir.join("desktop-2.dev.sock");
        let listener = UnixListener::bind(&serving).unwrap_or_else(|e| panic!("{e}"));
        let server = std::thread::spawn(move || {
            // One probe from `find`, then the request.
            for (n, stream) in listener.incoming().take(2).enumerate() {
                let stream = stream.unwrap_or_else(|e| panic!("{e}"));
                if n == 0 {
                    continue;
                }
                let mut line = String::new();
                BufReader::new(&stream)
                    .read_line(&mut line)
                    .unwrap_or_else(|e| panic!("{e}"));
                let mut writer = &stream;
                writeln!(writer, r#"{{"ok":false,"error":"no thanks"}}"#)
                    .unwrap_or_else(|e| panic!("{e}"));
            }
        });
        let found = find(&dir.join("server.sock")).unwrap_or_else(|r| panic!("{}", r.err.message));
        assert_eq!(found, serving);
        assert!(
            !dir.join("desktop-1.dev.sock").exists(),
            "the dead desktop's socket is pruned"
        );
        let refused = ask(&found, &json!({ "op": "perf" }), Duration::from_secs(5))
            .err()
            .unwrap_or_else(|| panic!("an ok:false reply is a refusal"));
        assert_eq!(refused.err.code, "desktop_refused");
        assert_eq!(refused.err.message, "no thanks");
        server.join().unwrap_or_else(|_| panic!("server thread"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
