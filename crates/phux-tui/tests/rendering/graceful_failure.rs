//! Regression guard: the attach loop must not enter the alt screen (or hide
//! the cursor) when it cannot even reach the server. It once flipped the
//! terminal before connecting, flashing the alt screen and, on SIGTERM
//! mid-connect, leaving the terminal wedged.

use std::path::PathBuf;
use std::time::Duration;

use phux_protocol::wire::frame::AttachTarget;
use phux_tui::attach::{self, AttachError};

/// A hang guard only; the assertions are on the error and the bytes.
const NO_HANG_DEADLINE: Duration = Duration::from_secs(30);

/// A missing socket and an impossible parent directory both fail with
/// `AttachError::Io` and write nothing to stdout.
#[test]
fn no_alt_screen_on_pre_handshake_failure() {
    let missing = PathBuf::from(format!(
        "/tmp/phux-roz-regress-{}-{}.sock",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    ));
    let _ = std::fs::remove_file(&missing);
    let impossible = PathBuf::from("/this/path/does/not/exist/phux-roz.sock");

    for (socket, target) in [
        (missing, AttachTarget::ByName("nonexistent".to_owned())),
        (impossible, AttachTarget::Last),
    ] {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build runtime");
        let mut captured: Vec<u8> = Vec::new();
        let result = rt.block_on(async {
            tokio::time::timeout(
                NO_HANG_DEADLINE,
                attach::run_with_stdout(&socket, target, &mut captured),
            )
            .await
        });
        let err = result
            .expect("attach hung instead of failing the connect")
            .expect_err("attach against an unreachable socket must fail");
        assert!(matches!(err, AttachError::Io(_)), "{socket:?}: {err:?}");
        assert!(
            captured.is_empty(),
            "{socket:?}: pre-handshake failure wrote {captured:?}"
        );
    }
}
