use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::attach::AttachError;
use phux_client::attach::connection::Connection;
use phux_protocol::wire::frame::{Command as WireCommand, CommandResult};
use phux_server::runtime::default_socket_path;

use crate::commands::{cli_runtime, command_on, report_no_server};

/// `phux upgrade` — ask the running server to graceful-upgrade itself in place
/// (ADR-0032).
///
/// The server snapshots every pane, re-execs the on-disk binary, and re-adopts
/// the live PTYs, so the shells / editors / agents in every session survive
/// the binary update. Attached clients (including this one's siblings) see a
/// brief disconnect and reconnect. Exit codes: 0 on a clean ack or the
/// expected re-exec disconnect, 1 on no server, 2 on a server-side refusal.
pub(crate) fn run_upgrade(socket: Option<PathBuf>) -> ExitCode {
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };

    rt.block_on(async move {
        let mut conn = match Connection::connect(&socket_path).await {
            Ok(conn) => conn,
            Err(err) => return report_no_server(&err, &socket_path, "upgrade"),
        };

        match command_on(&mut conn, 0, WireCommand::Upgrade).await {
            // `Ok` is the pre-exec ack. A `Disconnected` immediately after is
            // the expected blink as the old image is replaced — both mean the
            // upgrade is under way.
            Ok(CommandResult::Ok) | Err(AttachError::Disconnected) => {
                eprintln!("phux: server upgrading in place; sessions preserved");
                ExitCode::SUCCESS
            }
            Ok(CommandResult::Error { message, .. }) => {
                eprintln!("phux: upgrade refused: {message}");
                ExitCode::from(2)
            }
            Ok(other) => {
                eprintln!(
                    "phux: {}",
                    phux_client::explain::explain_unexpected("upgrade", &other)
                );
                ExitCode::from(2)
            }
            Err(err) => report_no_server(&err, &socket_path, "upgrade"),
        }
    })
}
