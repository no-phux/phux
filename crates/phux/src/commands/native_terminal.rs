//! One native emulator window per server-owned terminal.
use phux_client::{attach::connection::Connection, org, session, terminal};
use phux_protocol::{
    api::{TerminalCreate, TerminalPlacement},
    ids::ResourceId,
    wire::frame::OrgFilter,
};
use std::{
    io::{self, IsTerminal},
    path::PathBuf,
    process::ExitCode,
};
use usage::{Args, Subcommands};

/// Run one terminal in a native emulator window
///
/// Ctrl-] detaches; the server-owned process survives.
#[derive(Debug, Args)]
pub(crate) struct NativeArgs {
    #[usage(subcommand)]
    action: TerminalAction,
}

#[derive(Debug, Subcommands)]
enum TerminalAction {
    /// Create a distinct terminal in a shared persistent session and display it.
    Create(CreateArgs),
    /// Reattach an exact local terminal; never selects a focused pane.
    Attach(AttachArgs),
}

#[derive(Debug, Args)]
struct CreateArgs {
    /// Shared session, created persistently if missing.
    #[usage(short = 's', long, default = "omarchy")]
    session: String,
    /// Working directory (defaults to this client's current directory).
    #[usage(short = 'c', long)]
    cwd: Option<PathBuf>,
    /// Program and arguments; omitted selects the server's shell.
    #[usage(trailing_var_arg = true)]
    command: Vec<String>,
}

#[derive(Debug, Args)]
struct AttachArgs {
    /// Exact terminal address, as printed by `phux ls omarchy` (for example @3).
    target: String,
}

pub(crate) fn run(args: NativeArgs, socket: Option<PathBuf>) -> ExitCode {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        error!("terminal requires interactive stdin and stdout");
        return ExitCode::from(2);
    }
    let socket = socket.unwrap_or_else(phux_server::runtime::default_socket_path);
    if let Err(code) = super::attach::nested_attach_guard(&socket) {
        return code;
    }
    let result = (|| {
        if matches!(&args.action, TerminalAction::Create(_)) {
            super::server::ensure_server_unseeded(&socket, true)?;
        }
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let result = rt.block_on(async {
            let mut conn = phux_tui::attach::driver::native::connect(&socket).await?;
            let id = match args.action {
                TerminalAction::Attach(args) => parse_target(&args.target)?,
                TerminalAction::Create(args) => create(&mut conn, args).await?,
            };
            eprintln!("phux native {id} (Ctrl-] detaches)");
            phux_tui::attach::driver::native::run(&mut conn, id).await?;
            conn.shutdown().await;
            Ok::<_, Box<dyn std::error::Error>>(())
        });
        // The shared stdin fallback may have a blocking read in flight.
        // Detach must not wait for another key before the runtime can exit.
        rt.shutdown_timeout(std::time::Duration::from_millis(100));
        result
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            error!("{err}");
            ExitCode::FAILURE
        }
    }
}

fn parse_target(target: &str) -> Result<ResourceId, Box<dyn std::error::Error>> {
    let id = target
        .strip_prefix('@')
        .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|id| *id > 0)
        .ok_or("expected an exact local terminal address @N")?;
    Ok(ResourceId::Local { id })
}

async fn create(
    conn: &mut Connection,
    args: CreateArgs,
) -> Result<ResourceId, Box<dyn std::error::Error>> {
    phux_client::rename::check_session_name(&args.session)
        .map_err(|e| format!("invalid session: {e:?}"))?;
    let inventory = org::fetch(conn, OrgFilter::All, 1).await?;
    if inventory.session_named(&args.session).is_none() {
        // Concurrent windows may race to create the session. Refetch and
        // require the named record rather than treating any refusal as success.
        let _outcome = session::create_empty_session(conn, &args.session, &mut Vec::new()).await?;
    }
    let inventory = org::fetch(conn, OrgFilter::All, 1).await?;
    let session = inventory
        .session_named(&args.session)
        .ok_or("shared session was not created")?;
    let cwd = args.cwd.unwrap_or(std::env::current_dir()?);
    let cwd = std::fs::canonicalize(cwd)?.to_string_lossy().into_owned();
    let created = terminal::create(
        conn,
        &TerminalCreate {
            idempotency_key: uuid::Uuid::new_v4().simple().to_string(),
            session: session.key.to_string(),
            argv: args.command,
            cwd: Some(cwd),
            env: std::collections::BTreeMap::new(),
            name: None,
            placement: TerminalPlacement::NewGroup,
        },
    )
    .await?;
    Ok(ResourceId::Local {
        id: created
            .terminal_id
            .ok_or("created terminal has no live handle")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_exact_local_targets_are_accepted() {
        assert_eq!(parse_target("@42").unwrap(), ResourceId::Local { id: 42 });
        for bad in [
            "",
            "42",
            "@0",
            "@-1",
            "omarchy",
            "@1x",
            "@+1",
            "@4294967296",
        ] {
            assert!(parse_target(bad).is_err(), "{bad}");
        }
    }
}
