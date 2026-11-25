//! `phux open-url`: ask the person typing in this pane to open a link on
//! their own machine (`docs/ssh.md#opening-links`).

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use phux_client::attach::connection::Connection;
use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::ids::ResourceId;
use phux_protocol::open_url::{OPEN_URL_SECS, OpenUrlOp};
use phux_protocol::wire::frame::{Command, CommandResult};

use crate::commands::ctx;
use crate::commands::json_err::{Refusal, codes};
use crate::exit_codes::{EXIT_FAILURE, EXIT_USAGE};

/// The server answers within [`OPEN_URL_SECS`]; the rest is for the trip.
const REPLY_DEADLINE: Duration = Duration::from_secs(OPEN_URL_SECS + 10);

pub(crate) fn run(args: &crate::commands::OpenUrlArgs, socket: Option<PathBuf>) -> ExitCode {
    ctx::run_local(socket, false, |ctx| {
        let pane = crate::environment::own_terminal_id_on(ctx.socket()).ok_or_else(|| {
            let remedy = "run it in a pane of the server it talks to";
            Refusal::new(
                codes::USAGE_ERROR,
                "not running in a phux pane",
                remedy,
                EXIT_USAGE,
            )
        })?;
        ctx.rt.block_on(async {
            let unreachable = |err| ctx.unreachable(&err, "open-url");
            let mut conn = Connection::connect(ctx.socket())
                .await
                .map_err(unreachable)?;
            if !conn
                .server_features_ext()
                .contains(ServerFeatureExt::OpenUrl)
            {
                return Err(refused(
                    codes::SERVER_TOO_OLD,
                    "this server can't open links on a client",
                ));
            }
            // A person answers this one.
            conn.set_reply_deadline(Some(REPLY_DEADLINE));
            let command = Command::OpenUrl {
                terminal_id: ResourceId::local(pane),
                op: OpenUrlOp::Request {
                    url: args.url.clone(),
                },
            };
            let reply = conn.request(1, command).await.map_err(unreachable)?;
            match reply.into_parts().0 {
                CommandResult::Error { message, .. } => {
                    Err(refused(codes::TRANSPORT, &format!("not opened: {message}")))
                }
                _ => Ok(ExitCode::SUCCESS),
            }
        })
    })
}

fn refused(code: &'static str, message: &str) -> Refusal {
    Refusal::new(code, message, "", EXIT_FAILURE)
}
