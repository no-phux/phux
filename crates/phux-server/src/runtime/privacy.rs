//! `PRIVACY_STATUS`: the macOS privacy grants this server process holds.
//!
//! Only the server can answer: macOS attributes every pane's programs to it
//! (or to the app responsible for it), so a CLI probing its own grants
//! would report the terminal's. Every probe is a read that never prompts:
//!
//! - Full Disk Access: open the user's `TCC.db` read-only; a refusal means
//!   denied.
//! - Screen Recording: `CGPreflightScreenCaptureAccess`.
//! - Accessibility: `AXIsProcessTrusted`.
//!
//! The two preflights answer yes or no, so "denied" also covers "never
//! asked". Off macOS every status is `unknown`.

use std::path::{Path, PathBuf};

use phux_protocol::policy::TransportType;
use phux_protocol::wire::frame::{CommandResult, CommandValue, ErrorCode};
use tracing::warn;

use crate::state::{ClientId, ServerState, SharedState};

/// Answer `PRIVACY_STATUS` for `client_id`: refused off the Unix socket,
/// since which grants the host holds says what its panes can read.
pub(super) fn handle(state: &SharedState, client_id: ClientId) -> CommandResult {
    if !state.with(|s| from_local_socket(s, client_id)) {
        warn!(?client_id, "PRIVACY_STATUS refused: local socket only");
        return CommandResult::Error {
            code: ErrorCode::PermissionDenied,
            message: "PRIVACY_STATUS is accepted on the local socket only".to_owned(),
        };
    }
    CommandResult::OkWith(CommandValue::Json(report().to_string()))
}

fn from_local_socket(state: &ServerState, client_id: ClientId) -> bool {
    matches!(
        state.peer_identity(client_id).map(|peer| peer.transport),
        Some(TransportType::UnixSocket)
    )
}

/// The answer: `{"executable", "responsible_path", "permissions": [{"service",
/// "status"}]}`, status one of `granted`, `denied`, `not_determined`,
/// `unknown`.
fn report() -> serde_json::Value {
    let executable = phux_config::instance::running_executable().ok();
    let [fda, screen, accessibility] = probe::statuses();
    serde_json::json!({
        "executable": executable,
        "responsible_path": probe::responsible_path().map(|path| enclosing_app(&path)),
        "permissions": [
            {"service": "full_disk_access", "status": fda},
            {"service": "screen_recording", "status": screen},
            {"service": "accessibility", "status": accessibility},
        ],
    })
}

/// The outermost `.app` bundle `path` sits in (what System Settings lists
/// and what to drag in), else `path` itself.
fn enclosing_app(path: &Path) -> PathBuf {
    let mut bundle = PathBuf::new();
    for component in path.components() {
        bundle.push(component);
        if Path::new(component.as_os_str())
            .extension()
            .is_some_and(|ext| ext == "app")
        {
            return bundle;
        }
    }
    path.to_path_buf()
}

#[cfg(target_os = "macos")]
mod probe {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;
    use std::path::{Path, PathBuf};

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        safe fn CGPreflightScreenCaptureAccess() -> bool;
    }

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        safe fn AXIsProcessTrusted() -> bool;
    }

    const fn yes_no(granted: bool) -> &'static str {
        if granted { "granted" } else { "denied" }
    }

    /// Full Disk Access, Screen Recording, Accessibility.
    pub(super) fn statuses() -> [&'static str; 3] {
        let fda = std::env::var_os("HOME").map_or("unknown", |home| {
            full_disk_access(
                &Path::new(&home).join("Library/Application Support/com.apple.TCC/TCC.db"),
            )
        });
        [
            fda,
            yes_no(CGPreflightScreenCaptureAccess()),
            yes_no(AXIsProcessTrusted()),
        ]
    }

    fn full_disk_access(tcc_db: &Path) -> &'static str {
        match std::fs::File::open(tcc_db) {
            Ok(_) => "granted",
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => "denied",
            Err(_) => "unknown",
        }
    }

    /// The program macOS charges this process's privacy requests to: itself
    /// under launchd, or the app that started it (a terminal running
    /// `phux`). The lookup is a private libsystem call, so it is resolved at
    /// run time and its absence is `None`.
    pub(super) fn responsible_path() -> Option<PathBuf> {
        type Responsible = unsafe extern "C" fn(libc::pid_t) -> libc::pid_t;
        // SAFETY: a symbol lookup in the already-loaded images; a null
        // answer is handled.
        let symbol = unsafe {
            libc::dlsym(
                libc::RTLD_DEFAULT,
                c"responsibility_get_pid_responsible_for_pid".as_ptr(),
            )
        };
        if symbol.is_null() {
            return None;
        }
        // SAFETY: the symbol has had this signature since macOS 10.14.
        let responsible: Responsible = unsafe { std::mem::transmute(symbol) };
        let own = libc::pid_t::try_from(std::process::id()).ok()?;
        // SAFETY: takes and returns a pid; no memory is shared.
        let pid = unsafe { responsible(own) };
        if pid <= 0 {
            return None;
        }
        let mut buf = vec![0_u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        let len = u32::try_from(buf.len()).ok()?;
        // SAFETY: `buf` is writable for `len` bytes.
        let written = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), len) };
        buf.truncate(usize::try_from(written).ok().filter(|&n| n > 0)?);
        Some(PathBuf::from(OsString::from_vec(buf)))
    }
}

#[cfg(not(target_os = "macos"))]
mod probe {
    pub(super) const fn statuses() -> [&'static str; 3] {
        ["unknown"; 3]
    }

    pub(super) const fn responsible_path() -> Option<std::path::PathBuf> {
        None
    }
}

#[cfg(test)]
mod tests {
    use phux_protocol::policy::PeerIdentity;

    use super::*;

    fn peer(transport: TransportType) -> PeerIdentity {
        PeerIdentity {
            uid: nix::unistd::geteuid().as_raw(),
            pid: None,
            exe_path: None,
            mcp_host_key: None,
            transport,
            source_addr: None,
        }
    }

    #[test]
    fn only_the_unix_socket_may_ask() {
        let mut state = ServerState::new();
        let (local, quic, ws, unknown) = (ClientId(1), ClientId(2), ClientId(3), ClientId(4));
        state.set_peer_identity(local, peer(TransportType::UnixSocket));
        state.set_peer_identity(quic, peer(TransportType::Quic));
        state.set_peer_identity(ws, peer(TransportType::WebSocket));
        assert!(from_local_socket(&state, local));
        for client in [quic, ws, unknown] {
            assert!(!from_local_socket(&state, client), "{client:?}");
        }
    }

    #[test]
    fn the_report_names_three_services_and_a_known_status_each() {
        let report = report();
        let services: Vec<&str> = report["permissions"]
            .as_array()
            .expect("an array")
            .iter()
            .map(|row| {
                let status = row["status"].as_str().expect("a status");
                assert!(
                    ["granted", "denied", "not_determined", "unknown"].contains(&status),
                    "{row}"
                );
                row["service"].as_str().expect("a service")
            })
            .collect();
        assert_eq!(
            services,
            ["full_disk_access", "screen_recording", "accessibility"]
        );
        assert!(report["executable"].is_string(), "{report}");
        assert!(report.get("responsible_path").is_some(), "{report}");
    }

    #[test]
    fn the_responsible_path_is_the_outermost_app() {
        assert_eq!(
            enclosing_app(Path::new(
                "/Applications/Ghostty.app/Contents/MacOS/ghostty"
            )),
            Path::new("/Applications/Ghostty.app")
        );
        assert_eq!(
            enclosing_app(Path::new("/usr/local/bin/phux")),
            Path::new("/usr/local/bin/phux")
        );
    }
}
