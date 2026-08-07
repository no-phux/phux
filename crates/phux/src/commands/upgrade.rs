use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_client::attach::AttachError;
use phux_client::attach::connection::Connection;
use phux_protocol::wire::frame::{Command as WireCommand, CommandResult};
use phux_server::runtime::default_socket_path;

use crate::commands::{cli_runtime, command_on, report_no_server};

/// What a server said when asked to graceful-upgrade.
///
/// Extracted from [`run_upgrade`] so `phux update` can drive the same
/// primitive after it has put a new binary on disk, without duplicating the
/// wire round-trip or the "a disconnect right after the ack is success"
/// subtlety. `phux upgrade` remains the low-level verb: it re-execs whatever
/// is already on disk and discovers, downloads, and verifies nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UpgradeAck {
    /// The server acked (or blinked) and is re-execing; panes survive.
    Upgrading,
    /// The server refused, with its reason.
    Refused(String),
    /// The server answered with a frame `upgrade` does not expect. Kept
    /// apart from [`Self::Refused`] so the two keep the distinct wording
    /// they have always had.
    Unexpected(String),
}

/// Ask the server at `socket_path` to graceful-upgrade in place.
///
/// The `Err` arm carries the transport failure unchanged so each caller can
/// render it its own way — `phux upgrade` with the long-standing multi-line
/// no-server diagnostic, `phux update` as one field of its report.
pub(crate) fn request_upgrade(socket_path: &Path) -> Result<UpgradeAck, AttachError> {
    let Ok(rt) = cli_runtime() else {
        return Err(AttachError::Io(std::io::Error::other(
            "could not build a tokio runtime for the upgrade request",
        )));
    };

    rt.block_on(async move {
        let mut conn = Connection::connect(socket_path).await?;
        match command_on(&mut conn, 0, WireCommand::Upgrade).await {
            // `Ok` is the pre-exec ack. A `Disconnected` immediately after is
            // the expected blink as the old image is replaced — both mean the
            // upgrade is under way.
            Ok(CommandResult::Ok) | Err(AttachError::Disconnected) => Ok(UpgradeAck::Upgrading),
            Ok(CommandResult::Error { message, .. }) => Ok(UpgradeAck::Refused(message)),
            Ok(other) => Ok(UpgradeAck::Unexpected(
                phux_client::explain::explain_unexpected("upgrade", &other),
            )),
            Err(err) => Err(err),
        }
    })
}

/// `phux upgrade` — ask the running server to graceful-upgrade itself in place
/// (ADR-0032).
///
/// The server snapshots every pane, re-execs the on-disk binary, and re-adopts
/// the live PTYs, so the shells / editors / agents in every session survive
/// the binary update. Attached clients (including this one's siblings) see a
/// brief disconnect and reconnect. Exit codes: 0 on a clean ack or the
/// expected re-exec disconnect, 1 on no server, 2 on a server-side refusal.
///
/// This verb does not fetch anything. `phux update` is the user-facing command
/// that discovers, downloads, verifies, and installs a release and then calls
/// this one.
pub(crate) fn run_upgrade(socket: Option<PathBuf>) -> ExitCode {
    let socket_path = socket.unwrap_or_else(default_socket_path);

    match request_upgrade(&socket_path) {
        Ok(UpgradeAck::Upgrading) => {
            eprintln!("phux: server upgrading in place; sessions preserved");
            ExitCode::SUCCESS
        }
        Ok(UpgradeAck::Refused(message)) => {
            eprintln!("phux: upgrade refused: {message}");
            ExitCode::from(2)
        }
        Ok(UpgradeAck::Unexpected(message)) => {
            eprintln!("phux: {message}");
            ExitCode::from(2)
        }
        Err(err) => report_no_server(&err, &socket_path, "upgrade"),
    }
}
