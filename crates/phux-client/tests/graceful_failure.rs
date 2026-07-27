//! Regression guard for `phux-roz`: the attach loop must not enter the
//! alt screen if it can't even reach the server.
//!
//! Before the `phux-roz` fix, `attach::run` flipped the outer terminal
//! into the alt screen + hid the cursor BEFORE attempting the UDS
//! connect. Any pre-handshake failure (no socket, bogus path, server
//! mid-restart) caused a brief alt-screen flash on success-then-fail
//! and — worse, on SIGTERM mid-connect — left the terminal wedged with
//! the alt-screen and hidden cursor still active.
//!
//! After the fix, [`phux_client::attach::run_with_stdout`] runs the
//! connect / HELLO / ATTACH / ATTACHED-wait sequence on the cooked
//! terminal and only installs the `RawModeGuard` once the server has
//! accepted the attach. This test drives the failure path against a
//! socket path that doesn't exist and asserts that the captured writer
//! received ZERO terminal-control bytes — specifically, no
//! `\x1b[?1049h` (alt-screen-enter) and no `\x1b[?25l` (hide-cursor).
//!
//! The deadline guards against accidentally introducing a hang (e.g. a
//! future refactor that swallows the connect error and loops). It is a hang
//! guard and nothing more — the assertions are on the returned error and on
//! the bytes written, never on latency — so it is sized to be unreachable
//! under load rather than hand-picked (phux-br1f).

use std::path::PathBuf;
use std::time::Duration;

use phux_client::attach::{self, AttachError};
use phux_protocol::wire::frame::AttachTarget;

/// Hang guard for the pre-handshake failure path. See the module docs: the
/// value is deliberately not load-bearing, and a real hang still fails the
/// run with the same message, just later.
const NO_HANG_DEADLINE: Duration = Duration::from_secs(30);

/// Pre-handshake failure (no socket file exists) must:
///   1. Return `AttachError::Io` rather than hang (in practice immediate).
///   2. Write NOTHING to the supplied stdout sink — in particular, no
///      alt-screen-enter sequence. That's the regression guard.
#[test]
fn no_alt_screen_on_missing_socket() {
    let bogus = PathBuf::from(format!(
        "/tmp/phux-roz-regress-{}-{}.sock",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    ));
    // Belt-and-suspenders: ensure the path absolutely does not exist.
    let _ = std::fs::remove_file(&bogus);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build runtime");

    let mut captured: Vec<u8> = Vec::new();

    let result = rt.block_on(async {
        tokio::time::timeout(
            NO_HANG_DEADLINE,
            attach::run_with_stdout(
                &bogus,
                AttachTarget::ByName("nonexistent".to_owned()),
                &mut captured,
            ),
        )
        .await
    });

    let result = result.expect("attach::run_with_stdout hung instead of failing the connect");
    let err = result.expect_err("attach against missing socket should fail");

    assert!(
        matches!(err, AttachError::Io(_)),
        "expected AttachError::Io for missing socket, got {err:?}",
    );

    // The regression guard. Pre-fix this would contain `\x1b[?1049h`.
    assert!(
        captured.is_empty(),
        "pre-handshake failure must not write to stdout; got {} bytes: {:?}",
        captured.len(),
        captured,
    );
    let alt_screen_enter = b"\x1b[?1049h";
    assert!(
        !captured
            .windows(alt_screen_enter.len())
            .any(|w| w == alt_screen_enter),
        "alt-screen-enter sequence written on failure path (phux-roz regression)",
    );
    let hide_cursor = b"\x1b[?25l";
    assert!(
        !captured
            .windows(hide_cursor.len())
            .any(|w| w == hide_cursor),
        "hide-cursor sequence written on failure path (phux-roz regression)",
    );
}

/// A non-`ConnectionRefused`/`NotFound` socket parent dir error (e.g.
/// `/nonexistent/dir/foo.sock`) should also short-circuit cleanly with
/// no alt-screen-enter byte. Guards against a future refactor that
/// only special-cases the two common kinds.
#[test]
fn no_alt_screen_on_bogus_parent_dir() {
    let bogus = PathBuf::from("/this/path/does/not/exist/phux-roz.sock");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build runtime");

    let mut captured: Vec<u8> = Vec::new();

    let result = rt.block_on(async {
        tokio::time::timeout(
            NO_HANG_DEADLINE,
            attach::run_with_stdout(&bogus, AttachTarget::Last, &mut captured),
        )
        .await
    });

    let result = result.expect("attach::run_with_stdout hung instead of failing the connect");
    let err = result.expect_err("attach against impossible path should fail");
    assert!(
        matches!(err, AttachError::Io(_)),
        "expected AttachError::Io, got {err:?}",
    );
    assert!(
        captured.is_empty(),
        "no bytes should be written for impossible-path failure; got {captured:?}",
    );
}
