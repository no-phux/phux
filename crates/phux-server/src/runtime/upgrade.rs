//! Graceful-upgrade orchestration (ADR-0032): build the handoff blob, clear
//! `FD_CLOEXEC` on inherited descriptors, validate the on-disk binary, and
//! re-exec it as `server --resume <fd>` plus the effective runtime flags
//! (`--listen` / `--quic` / `--webtransport` / `--connect` / `--hub`) so the
//! resumed image serves the same surface the old one did.
//!
//! Split into [`prepare_upgrade`] (everything reversible — if it fails the old
//! image keeps serving and no child is stranded) and [`UpgradePlan::exec`]
//! (the irreversible re-exec). The caller acks the client between the two.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, BorrowedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use tokio::sync::oneshot;

use super::RuntimeFlags;
use crate::state::SharedState;
use crate::terminal_actor::UpgradeHandleRequest;

/// Errors preparing a graceful upgrade. Any of these leaves the running server
/// untouched (the children are never stranded — see the module docs).
#[derive(Debug, thiserror::Error)]
pub(super) enum UpgradeError {
    /// The server hasn't captured its upgrade context yet (not serving).
    #[error("server not ready for upgrade (no listener context)")]
    NoContext,
    /// The handoff blob could not be serialized.
    #[error("serialize handoff blob: {0}")]
    Blob(#[from] crate::upgrade::blob::BlobError),
    /// A descriptor / temp-file operation failed.
    #[error("upgrade io: {0}")]
    Io(#[from] std::io::Error),
    /// The on-disk binary failed its pre-commit validation, so the upgrade is
    /// aborted before anything irreversible happens.
    #[error("new binary failed validation: {0}")]
    Validation(String),
}

/// A validated, ready-to-`exec` upgrade. Holds the open blob temp file so its
/// fd stays valid until the re-exec consumes it.
pub(super) struct UpgradePlan {
    current_exe: PathBuf,
    blob_fd: RawFd,
    socket_path: PathBuf,
    /// The server's effective runtime flags (phux-v45.10), read back from the
    /// upgrade context the runtime captured at startup. Re-emitted on the
    /// resume argv so `--listen` / `--quic` / `--webtransport` / `--connect`
    /// / `--hub` survive the re-exec.
    flags: RuntimeFlags,
    _blob_file: std::fs::File,
}

/// Do everything reversible: snapshot the tree into a handoff blob, stage it in
/// an inheritable temp file, clear `FD_CLOEXEC` on the blob / listener / every
/// pane master, and validate the on-disk binary. Returns a [`UpgradePlan`] the
/// caller execs *after* acking the client.
pub(super) async fn prepare_upgrade(state: &SharedState) -> Result<UpgradePlan, UpgradeError> {
    let (listener_fd, socket_path, flags) = state
        .with(|s| {
            s.upgrade_context()
                .map(|(fd, path, flags)| (fd, path.to_path_buf(), flags))
        })
        .ok_or(UpgradeError::NoContext)?;

    // Gather each pane's handoff out of lock (the state lock can't be held
    // across the await), then assemble the blob back under the lock.
    let handles = state.with(crate::state::ServerState::upgrade_handles);
    let mut handoffs = HashMap::new();
    for (tid, handle) in handles {
        let (reply, rx) = oneshot::channel();
        if handle
            .upgrade
            .send(UpgradeHandleRequest { reply })
            .await
            .is_ok()
            && let Ok(handoff) = rx.await
        {
            handoffs.insert(tid, handoff);
        }
    }
    let blob = state.with(|s| s.assemble_upgrade_blob(listener_fd, &handoffs));

    // Stage the blob in an anonymous temp file (auto-removed on close), rewound
    // so the resumed image reads from the start.
    let mut blob_file = tempfile::tempfile()?;
    blob_file.write_all(&blob.to_bytes()?)?;
    blob_file.seek(SeekFrom::Start(0))?;
    let blob_fd = blob_file.as_raw_fd();

    // Everything the re-exec'd image must inherit needs FD_CLOEXEC cleared.
    clear_cloexec(blob_fd)?;
    clear_cloexec(listener_fd)?;
    for pane in &blob.panes {
        if let Some(master_fd) = pane.master_fd {
            clear_cloexec(master_fd)?;
        }
    }

    // Pre-commit safety: refuse to re-exec a binary that can't even print its
    // version, so a half-written `cargo install` can't strand the session.
    let current_exe = std::env::current_exe()?;
    validate_binary(&current_exe)?;

    Ok(UpgradePlan {
        current_exe,
        blob_fd,
        socket_path,
        flags,
        _blob_file: blob_file,
    })
}

impl UpgradePlan {
    /// Re-exec the new binary as `server --resume <blob_fd> --socket <path>`
    /// plus the effective runtime flags (`--listen` / `--quic` /
    /// `--webtransport` / `--hub`, phux-v45.10), replacing this process in
    /// place. Returns only on failure — and a failure is harmless: nothing
    /// was closed, so the old image keeps serving and the children stay
    /// attached.
    pub(super) fn exec(self) -> std::io::Error {
        Command::new(&self.current_exe)
            .args(resume_args(
                self.blob_fd,
                &self.socket_path,
                self.flags.clone(),
            ))
            .exec()
    }
}

/// Build the full argv (after argv0) for the graceful-upgrade re-exec:
/// `server --resume <blob_fd> --socket <path>` plus one entry per effective
/// runtime flag (phux-v45.10). Pure, so the reconstruction is testable
/// without exec'ing anything.
fn resume_args(blob_fd: RawFd, socket_path: &Path, flags: RuntimeFlags) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        OsString::from("server"),
        OsString::from("--resume"),
        OsString::from(blob_fd.to_string()),
        OsString::from("--socket"),
        socket_path.into(),
    ];
    if let Some(addr) = flags.ws_addr {
        args.push(OsString::from("--listen"));
        args.push(OsString::from(addr.to_string()));
    }
    if let Some(addr) = flags.quic_addr {
        args.push(OsString::from("--quic"));
        args.push(OsString::from(addr.to_string()));
    }
    if let Some(addr) = flags.wt_addr {
        args.push(OsString::from("--webtransport"));
        args.push(OsString::from(addr.to_string()));
    }
    if let Some(relay) = flags.connect {
        args.push(OsString::from("--connect"));
        args.push(OsString::from(relay));
    }
    if flags.hub {
        args.push(OsString::from("--hub"));
    }
    args
}

/// Clear `FD_CLOEXEC` so `fd` survives the `execve`.
fn clear_cloexec(fd: RawFd) -> std::io::Result<()> {
    use rustix::io::{FdFlags, fcntl_getfd, fcntl_setfd};
    // SAFETY: `fd` is open and owned by this process for the duration of the
    // two fcntl calls; we only borrow it, never take ownership.
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
    let flags = fcntl_getfd(borrowed)?;
    fcntl_setfd(borrowed, flags.difference(FdFlags::CLOEXEC))?;
    Ok(())
}

/// Validate the on-disk binary runs by probing `--version`.
fn validate_binary(exe: &Path) -> Result<(), UpgradeError> {
    let output = Command::new(exe).arg("--version").output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(UpgradeError::Validation(format!(
            "`{} --version` exited with {}",
            exe.display(),
            output.status
        )))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]

    use std::net::SocketAddr;

    use super::*;

    const WS: &str = "127.0.0.1:8787";
    const QUIC: &str = "0.0.0.0:4433";
    const WT: &str = "0.0.0.0:4434";

    fn flags(ws: Option<&str>, quic: Option<&str>, wt: Option<&str>, hub: bool) -> RuntimeFlags {
        let addr = |s: &str| s.parse::<SocketAddr>().unwrap();
        RuntimeFlags {
            ws_addr: ws.map(addr),
            quic_addr: quic.map(addr),
            wt_addr: wt.map(addr),
            connect: None,
            hub,
        }
    }

    fn args_as_strings(flags: RuntimeFlags) -> Vec<String> {
        resume_args(7, Path::new("/run/phux/phux.sock"), flags)
            .into_iter()
            .map(|a| a.into_string().unwrap())
            .collect()
    }

    /// The base of the resume argv is invariant: subcommand, blob fd, socket.
    const BASE: [&str; 5] = ["server", "--resume", "7", "--socket", "/run/phux/phux.sock"];

    /// phux-v45.10 regression matrix: every combination of the opt-in runtime
    /// flags must be reconstructed on the re-exec argv — the original bug was
    /// an argv of only `server --resume <fd> --socket <path>`, silently
    /// dropping `--listen`, `--quic`, and `--hub` across `phux server
    /// upgrade` (and later, in the same class, `--webtransport` — phux-0wmf).
    #[test]
    fn resume_args_reconstructs_every_flag_combination() {
        type Case<'a> = (
            Option<&'a str>,
            Option<&'a str>,
            Option<&'a str>,
            bool,
            &'a [&'a str],
        );
        let cases: [Case<'_>; 16] = [
            (None, None, None, false, &[]),
            (Some(WS), None, None, false, &["--listen", WS]),
            (None, Some(QUIC), None, false, &["--quic", QUIC]),
            (None, None, Some(WT), false, &["--webtransport", WT]),
            (None, None, None, true, &["--hub"]),
            (
                Some(WS),
                Some(QUIC),
                None,
                false,
                &["--listen", WS, "--quic", QUIC],
            ),
            (
                Some(WS),
                None,
                Some(WT),
                false,
                &["--listen", WS, "--webtransport", WT],
            ),
            (Some(WS), None, None, true, &["--listen", WS, "--hub"]),
            (
                None,
                Some(QUIC),
                Some(WT),
                false,
                &["--quic", QUIC, "--webtransport", WT],
            ),
            (None, Some(QUIC), None, true, &["--quic", QUIC, "--hub"]),
            (None, None, Some(WT), true, &["--webtransport", WT, "--hub"]),
            (
                Some(WS),
                Some(QUIC),
                Some(WT),
                false,
                &["--listen", WS, "--quic", QUIC, "--webtransport", WT],
            ),
            (
                Some(WS),
                Some(QUIC),
                None,
                true,
                &["--listen", WS, "--quic", QUIC, "--hub"],
            ),
            (
                Some(WS),
                None,
                Some(WT),
                true,
                &["--listen", WS, "--webtransport", WT, "--hub"],
            ),
            (
                None,
                Some(QUIC),
                Some(WT),
                true,
                &["--quic", QUIC, "--webtransport", WT, "--hub"],
            ),
            (
                Some(WS),
                Some(QUIC),
                Some(WT),
                true,
                &[
                    "--listen",
                    WS,
                    "--quic",
                    QUIC,
                    "--webtransport",
                    WT,
                    "--hub",
                ],
            ),
        ];
        for (ws, quic, wt, hub, extra) in cases {
            let mut expected: Vec<String> = BASE.iter().map(ToString::to_string).collect();
            expected.extend(extra.iter().map(ToString::to_string));
            assert_eq!(
                args_as_strings(flags(ws, quic, wt, hub)),
                expected,
                "argv mismatch for ws={ws:?} quic={quic:?} wt={wt:?} hub={hub}",
            );
        }
    }

    /// The default (UDS-only, non-hub) server re-execs with the bare argv —
    /// no spurious flags invented for surfaces it never served.
    #[test]
    fn resume_args_default_flags_add_nothing() {
        assert_eq!(args_as_strings(RuntimeFlags::default()), BASE);
    }

    #[test]
    fn resume_args_preserves_ad_hoc_connector() {
        let flags = RuntimeFlags {
            connect: Some("relay.example:4433".to_owned()),
            ..RuntimeFlags::default()
        };
        let mut expected: Vec<String> = BASE.iter().map(ToString::to_string).collect();
        expected.extend(["--connect".to_owned(), "relay.example:4433".to_owned()]);
        assert_eq!(args_as_strings(flags), expected);
    }

    /// The flags land in the plan from the shared-state upgrade context —
    /// the same channel `prepare_upgrade` reads — not from anywhere argv-ish.
    #[test]
    fn upgrade_context_round_trips_runtime_flags() {
        let state = SharedState::new();
        assert!(
            state.with(|s| s.upgrade_context().is_none()),
            "no context before serving"
        );
        let captured = flags(Some(WS), Some(QUIC), Some(WT), true);
        state.with_mut(|s| {
            s.set_upgrade_context(3, PathBuf::from("/tmp/phux.sock"), captured.clone());
        });
        let (fd, path, roundtripped) = state
            .with(|s| {
                s.upgrade_context()
                    .map(|(fd, path, flags)| (fd, path.to_path_buf(), flags))
            })
            .expect("context set");
        assert_eq!(fd, 3);
        assert_eq!(path, PathBuf::from("/tmp/phux.sock"));
        assert_eq!(roundtripped, captured);
    }
}
