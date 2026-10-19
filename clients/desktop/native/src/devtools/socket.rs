//! The dev socket: the desktop's telemetry as line JSON, for `phux perf
//! --desktop`, agents, and `nc -U`.
//!
//! One request per line, one response per line:
//!
//! ```text
//! {"op":"perf"}                         full report, frame summary, HUD state
//! {"op":"frames","n":120}               the last n frames: age, build, gap
//! {"op":"server"}                       the server's report via `phux perf --json`
//! {"op":"trace","seconds":5,"server":true}  Perfetto capture; returns paths
//! {"op":"hud","on":true}                show or hide the HUD
//! {"op":"alloc","on":true}              heap accounting on or off
//! {"op":"reset"}                        zero every desktop histogram and counter
//! {"op":"help"}                         this list
//! ```
//!
//! Every response is `{"ok":true,...}` or `{"ok":false,"error":"..."}`. The
//! socket is created `0600` and carries no terminal content.

use std::io::{self, BufRead as _, BufReader, Write as _};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Environment override for the socket path.
pub const PATH_ENV: &str = "PHUX_DESKTOP_DEV_SOCKET";

/// Longest trace a request may ask for.
const MAX_TRACE: Duration = Duration::from_secs(60);
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_REQUEST_BYTES: usize = 64 * 1024;
const MAX_CONNECTIONS: usize = 16;
static CONNECTIONS: AtomicUsize = AtomicUsize::new(0);

/// `desktop-<pid>.dev.sock` beside the server socket, so `phux perf
/// --desktop` finds it for the same profile; the temp directory when no
/// server socket is known. [`PATH_ENV`] overrides both.
///
/// # Errors
///
/// The fallback private directory could not be created or verified.
pub fn path_for(server_socket: Option<&Path>) -> anyhow::Result<PathBuf> {
    if let Some(path) = std::env::var_os(PATH_ENV) {
        return Ok(PathBuf::from(path));
    }
    let name = format!("desktop-{}.dev.sock", std::process::id());
    server_socket
        .and_then(Path::parent)
        .filter(|dir| dir.is_dir())
        .map_or_else(
            || Ok(private_temp_dir()?.join(&name)),
            |dir| Ok(dir.join(&name)),
        )
}

/// A `0700` directory of this process's own under the temp directory, so
/// the socket is never briefly reachable at the umask's mode in a shared
/// `/tmp` between `bind` and `chmod`.
fn private_temp_dir() -> anyhow::Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt as _;
    let dir = std::env::temp_dir().join(format!("phux-desktop-{}", std::process::id()));
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    validate_directory(&dir)?;
    Ok(dir)
}

fn validate_directory(dir: &Path) -> anyhow::Result<()> {
    let meta = std::fs::symlink_metadata(dir)?;
    anyhow::ensure!(
        meta.is_dir() && meta.uid() == nix::unistd::geteuid().as_raw() && meta.mode() & 0o077 == 0,
        "dev socket parent {} must be a non-symlink, owner-only directory owned by this user",
        dir.display()
    );
    Ok(())
}

fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    #[cfg(target_os = "macos")]
    let uid = nix::unistd::getpeereid(stream).map(|(uid, _)| uid.as_raw());
    #[cfg(target_os = "linux")]
    let uid = nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::PeerCredentials)
        .map(|credentials| credentials.uid());
    uid.map_err(io::Error::from)
}

fn owner_peer(stream: &UnixStream) -> bool {
    peer_uid(stream).is_ok_and(|uid| uid == nix::unistd::geteuid().as_raw())
}

/// Remove `path` only when it is a socket nobody answers on: a desktop that
/// died with this pid. Anything else at the path is left alone, so a
/// mistaken override cannot delete the server's socket or a file.
fn remove_stale_socket(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::FileTypeExt as _;
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return Ok(());
    };
    if !meta.file_type().is_socket() {
        anyhow::bail!("{} exists and is not a socket", path.display());
    }
    match UnixStream::connect(path) {
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {}
        Ok(_) => anyhow::bail!("{} is already being served", path.display()),
        Err(error) => return Err(error.into()),
    }
    std::fs::remove_file(path)?;
    Ok(())
}

/// Bind `path` and serve it on a background thread. Returns the bound path.
///
/// # Errors
///
/// The path could not be bound.
pub fn serve(path: &Path) -> anyhow::Result<PathBuf> {
    phux_config_core::socket::refuse_dev_on_production(path).map_err(anyhow::Error::msg)?;
    validate_directory(path.parent().unwrap_or_else(|| Path::new(".")))?;
    remove_stale_socket(path)?;
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    std::thread::Builder::new()
        .name("phux-devtools-socket".to_owned())
        .spawn(move || accept(&listener))?;
    Ok(path.to_path_buf())
}

fn accept(listener: &UnixListener) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if !owner_peer(&stream) {
                    continue;
                }
                let Some(permit) = ConnectionPermit::take() else {
                    continue;
                };
                let spawned = std::thread::Builder::new()
                    .name("phux-devtools-conn".to_owned())
                    .spawn(move || {
                        let _permit = permit;
                        serve_connection(stream);
                    });
                if let Err(error) = spawned {
                    log::warn!("dev socket: could not serve a connection: {error}");
                }
            }
            Err(error) => log::warn!("dev socket: accept failed: {error}"),
        }
    }
}

struct ConnectionPermit;

impl ConnectionPermit {
    fn take() -> Option<Self> {
        CONNECTIONS
            .try_update(Relaxed, Relaxed, |count| {
                (count < MAX_CONNECTIONS).then_some(count + 1)
            })
            .ok()
            .map(|_| Self)
    }
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        CONNECTIONS.fetch_sub(1, Relaxed);
    }
}

fn serve_connection(stream: UnixStream) {
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    if writer.set_write_timeout(Some(IO_TIMEOUT)).is_err() {
        return;
    }
    let mut reader = BufReader::new(stream);
    for _ in 0..64 {
        let line = match read_request(&mut reader, IO_TIMEOUT) {
            Ok(line) if line.is_empty() => return,
            Ok(line) => line,
            Err(error) => {
                let _ = writeln!(writer, "{}", failure(error.to_string()));
                return;
            }
        };
        let response = match serde_json::from_slice::<Value>(&line) {
            Ok(request) => handle(&request),
            Err(error) => failure(format!("request is not JSON: {error}")),
        };
        if writeln!(writer, "{response}").is_err() {
            return;
        }
    }
}

fn read_request(reader: &mut BufReader<UnixStream>, timeout: Duration) -> io::Result<Vec<u8>> {
    let deadline = Instant::now() + timeout;
    let mut line = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "request deadline exceeded",
            ));
        }
        reader.get_ref().set_read_timeout(Some(remaining))?;
        let bytes = reader.fill_buf()?;
        if bytes.is_empty() {
            return Ok(line);
        }
        let end = bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|index| index + 1);
        let count = end.unwrap_or(bytes.len());
        if line.len() + count > MAX_REQUEST_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request exceeds 65536 bytes",
            ));
        }
        line.extend_from_slice(&bytes[..count]);
        reader.consume(count);
        if end.is_some() {
            return Ok(line);
        }
    }
}

/// Answer one request.
#[must_use]
pub fn handle(request: &Value) -> Value {
    handle_with_cli(request, super::get().cli.as_ref())
}

fn handle_with_cli(request: &Value, cli: Option<&super::ServerCli>) -> Value {
    if !matches!(request["op"].as_str(), Some("identity" | "help") | None)
        && let Some(cli) = cli
        && let Err(error) = cli.check_socket()
    {
        return failure(error.to_string());
    }
    match request["op"].as_str().unwrap_or_default() {
        "identity" => {
            json!({ "ok": true, "server_socket": super::get().cli.as_ref().map(super::ServerCli::socket) })
        }
        "perf" => perf(request["reset"].as_bool().unwrap_or(false)),
        "frames" => frames(request["n"].as_u64()),
        "server" => server(),
        "trace" => trace(request),
        "hud" => toggle(request, super::hud_visible(), |on| {
            super::set_hud(on);
            json!({ "ok": true, "hud": on })
        }),
        "alloc" => toggle(request, phux_perf::alloc::enabled(), |on| {
            if on {
                phux_perf::alloc::enable();
            } else {
                phux_perf::alloc::disable();
            }
            json!({ "ok": true, "alloc": on, "installed": phux_perf::alloc::installed() })
        }),
        "reset" => {
            for table in super::tables() {
                phux_perf::reset(table);
            }
            json!({ "ok": true })
        }
        "help" | "" => json!({
            "ok": true,
            "ops": ["perf", "frames", "server", "trace", "hud", "alloc", "reset", "help"],
        }),
        other => failure(format!("unknown op {other:?}; try {{\"op\":\"help\"}}")),
    }
}

fn failure(error: impl Into<String>) -> Value {
    json!({ "ok": false, "error": error.into() })
}

/// `on` from the request, or the opposite of `current` when it is absent.
fn toggle(request: &Value, current: bool, apply: impl FnOnce(bool) -> Value) -> Value {
    apply(request["on"].as_bool().unwrap_or(!current))
}

fn perf(reset: bool) -> Value {
    let snapshot = super::snapshot();
    if reset {
        for table in super::tables() {
            phux_perf::reset(table);
        }
    }
    json!({
        "ok": true,
        "report": snapshot.report,
        "frames": snapshot.frames,
        "hud": snapshot.hud,
        "dev": snapshot.dev,
        "socket": snapshot.socket,
        "physical_footprint_bytes": snapshot.physical_footprint_bytes,
        "native_heap": snapshot.native_heap,
        "server_socket": super::get().cli.as_ref().map(super::ServerCli::socket),
    })
}

fn frames(n: Option<u64>) -> Value {
    let n = n.map_or(120, |n| usize::try_from(n).unwrap_or(usize::MAX));
    let now = Instant::now();
    let frames: Vec<Value> = super::frames::last(n.min(super::frames::CAPACITY))
        .iter()
        .map(|frame| {
            json!({
                "ago_us": micros(now.saturating_duration_since(frame.at)),
                "build_us": micros(frame.build),
                "gap_us": frame.gap.map(micros),
            })
        })
        .collect();
    json!({ "ok": true, "frames": frames })
}

fn server() -> Value {
    let Some(cli) = &super::get().cli else {
        return failure("no phux CLI is known to this desktop");
    };
    match cli.report() {
        Ok(report) => json!({ "ok": true, "report": report }),
        Err(error) => failure(format!("{error:#}")),
    }
}

fn trace(request: &Value) -> Value {
    let seconds = request["seconds"].as_f64().unwrap_or(5.0);
    let Some(duration) = trace_duration(seconds) else {
        return failure("seconds must be positive and finite");
    };
    let with_server = request["server"].as_bool().unwrap_or(true);
    match super::capture_trace(duration, with_server) {
        Ok(result) => {
            let mut value = json!(result);
            value["ok"] = json!(true);
            value
        }
        Err(error) => failure(format!("{error:#}")),
    }
}

fn trace_duration(seconds: f64) -> Option<Duration> {
    if !seconds.is_finite() || seconds <= 0.0 {
        return None;
    }
    Duration::try_from_secs_f64(seconds.min(MAX_TRACE.as_secs_f64()))
        .ok()
        .filter(|duration| !duration.is_zero())
}

fn micros(d: Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod production_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_and_malformed_requests_fail_politely() {
        let reply = handle(&json!({ "op": "explode" }));
        assert_eq!(reply["ok"], false);
        assert!(reply["error"].as_str().unwrap_or_default().contains("help"));
        assert_eq!(handle(&json!({ "op": "help" }))["ok"], true);
        assert_eq!(handle(&json!({ "op": "trace", "seconds": 0 }))["ok"], false);
    }

    #[test]
    fn trace_duration_caps_huge_values_before_conversion() {
        for seconds in [1e300, f64::MAX, 61.0] {
            assert_eq!(trace_duration(seconds), Some(MAX_TRACE));
        }
        assert_eq!(trace_duration(0.25), Some(Duration::from_millis(250)));
        for seconds in [
            0.0,
            -1.0,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::MIN_POSITIVE,
        ] {
            assert_eq!(trace_duration(seconds), None);
        }
    }

    #[test]
    fn socket_refuses_untrusted_parents_and_checks_kernel_peer() {
        let dir = std::env::temp_dir().join(format!("phux-devsock-trust-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777))
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(serve(&dir.join("t.sock")).is_err());
        assert!(!dir.join("t.sock").exists());
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .unwrap_or_else(|e| panic!("{e}"));
        let link = dir.with_extension("link");
        std::os::unix::fs::symlink(&dir, &link).unwrap_or_else(|e| panic!("{e}"));
        assert!(serve(&link.join("t.sock")).is_err());
        let (a, b) = UnixStream::pair().unwrap_or_else(|e| panic!("{e}"));
        assert!(owner_peer(&a));
        assert!(owner_peer(&b));
        let _ = std::fs::remove_file(link);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn socket_requests_are_bounded_and_abandoned_reads_expire() {
        let (mut client, stream) = UnixStream::pair().unwrap_or_else(|e| panic!("{e}"));
        let worker = std::thread::spawn(move || serve_connection(stream));
        client
            .write_all(&vec![b'x'; MAX_REQUEST_BYTES + 1])
            .unwrap_or_else(|e| panic!("{e}"));
        let mut reply = String::new();
        BufReader::new(client)
            .read_line(&mut reply)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(reply.contains("exceeds 65536 bytes"), "{reply}");
        worker.join().unwrap_or_else(|_| panic!("worker"));
        let (_client, stream) = UnixStream::pair().unwrap_or_else(|e| panic!("{e}"));
        let error = read_request(&mut BufReader::new(stream), Duration::from_millis(20))
            .expect_err("an abandoned request expires");
        assert!(matches!(
            error.kind(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
        ));
        let mut permits = Vec::new();
        while let Some(permit) = ConnectionPermit::take() {
            permits.push(permit);
        }
        assert!(permits.len() <= MAX_CONNECTIONS);
        assert!(ConnectionPermit::take().is_none());
        drop(permits);
        assert!(ConnectionPermit::take().is_some());
    }

    #[test]
    fn perf_reset_returns_the_snapshot_before_clearing_metrics() {
        crate::diag::DRAIN_WAKES.add(100_000);
        let reply = handle(&json!({"op": "perf", "reset": true}));
        let metrics = reply["report"]["metrics"]
            .as_array()
            .unwrap_or_else(|| panic!("metrics"));
        let wakes = metrics
            .iter()
            .find(|row| row["name"] == "desktop.drain_wakes")
            .unwrap_or_else(|| panic!("wakes"));
        assert!(wakes["value"].as_u64().unwrap_or_default() >= 100_000);
        assert!(crate::diag::DRAIN_WAKES.get() < 100_000);
    }

    #[test]
    fn perf_and_frames_answer_over_a_real_socket() {
        let dir = std::env::temp_dir().join(format!("phux-devsock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .unwrap_or_else(|e| panic!("{e}"));
        let path = serve(&dir.join("t.sock")).unwrap_or_else(|e| panic!("{e}"));
        let mode = std::fs::metadata(&path)
            .unwrap_or_else(|e| panic!("{e}"))
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        drop(crate::devtools::frames::FrameTimer::start());
        let stream = UnixStream::connect(&path).unwrap_or_else(|e| panic!("{e}"));
        let mut writer = stream.try_clone().unwrap_or_else(|e| panic!("{e}"));
        let mut lines = BufReader::new(stream).lines();
        let mut ask = |request: Value| -> Value {
            writeln!(writer, "{request}").unwrap_or_else(|e| panic!("{e}"));
            let line = lines
                .next()
                .unwrap_or_else(|| panic!("no reply"))
                .unwrap_or_else(|e| panic!("{e}"));
            serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}"))
        };
        let perf = ask(json!({ "op": "perf" }));
        assert_eq!(perf["ok"], true);
        assert!(
            perf["report"]["metrics"]
                .as_array()
                .is_some_and(|m| !m.is_empty())
        );
        assert!(perf["frames"]["fps"].is_number());
        let frames = ask(json!({ "op": "frames", "n": 5 }));
        assert!(frames["frames"].as_array().is_some_and(|f| !f.is_empty()));
        assert!(serve(&path).is_err(), "a live socket is never replaced");
        let file = dir.join("not-a-socket");
        std::fs::write(&file, "keep").unwrap_or_else(|e| panic!("{e}"));
        assert!(serve(&file).is_err());
        assert!(file.exists(), "a non-socket at the path is left alone");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
