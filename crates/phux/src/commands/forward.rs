//! `phux forward -L`: listen here, carry each connection to a port on the
//! server's loopback (`docs/forwarding.md`). Runs until Ctrl-C.
//! `phux forward --agent` is in [`super::forward_agent`].
use std::process::ExitCode;
use std::sync::Arc;

use phux_client_runtime::control::forward::{self, ForwardError};
use phux_client_runtime::control::{ControlOptions, Event, Status};
use phux_client_runtime::runtime::{Client, ClientOptions, Listener, Runtime};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;

/// One `-L` argument: where to listen here and what to reach there.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Spec {
    bind: String,
    local_port: u16,
    host: String,
    port: u16,
}

/// Parse `[bind_addr:]local_port:remote_host:remote_port`, with an IPv6
/// address in brackets as ssh spells it.
fn parse(spec: &str) -> Result<Spec, String> {
    const USAGE: &str = "expected [BIND_ADDR:]LOCAL_PORT:REMOTE_HOST:REMOTE_PORT";
    let mut parts = Vec::new();
    let mut rest = spec;
    while !rest.is_empty() {
        let (part, tail) = match rest.strip_prefix('[') {
            Some(bracketed) => {
                let (addr, tail) = bracketed.split_once(']').ok_or(USAGE)?;
                (addr, tail)
            }
            None => rest.split_at(rest.find(':').unwrap_or(rest.len())),
        };
        parts.push(part);
        rest = match tail.strip_prefix(':') {
            Some(next) if !next.is_empty() => next,
            None if tail.is_empty() => "",
            _ => return Err(USAGE.to_owned()),
        };
    }
    let port = |text: &str| {
        text.parse::<u16>()
            .map_err(|_| format!("{text:?} is not a port; {USAGE}"))
    };
    let (bind, local_port, host, remote_port) = match parts[..] {
        [local, host, remote] => ("127.0.0.1", local, host, remote),
        [bind, local, host, remote] => (bind, local, host, remote),
        _ => return Err(USAGE.to_owned()),
    };
    if bind.is_empty() || host.is_empty() {
        return Err(USAGE.to_owned());
    }
    Ok(Spec {
        bind: bind.to_owned(),
        local_port: port(local_port)?,
        host: host.to_owned(),
        port: port(remote_port)?,
    })
}

pub(super) struct Activity(pub(super) Notify);

impl Listener for Activity {
    fn on_activity(&self) {
        self.0.notify_one();
    }
}

/// `-L local`, or with none `--agent` (`yes` skips its per-signature ask).
pub(crate) fn run(
    args: crate::commands::ForwardArgs,
    socket: Option<std::path::PathBuf>,
) -> ExitCode {
    let spec = args.remote.with_socket(socket);
    let local = args.local.as_deref();
    let yes = args.yes;
    let forward = match local.map(parse).transpose() {
        Ok(forward) => forward,
        Err(err) => {
            error!("-L {}: {err}", local.unwrap_or_default());
            return ExitCode::from(2);
        }
    };
    let host = spec.remote.clone();
    let (rt, server) = match spec.prepare("forward", false) {
        Ok(value) => value,
        Err(code) => return code,
    };
    let options = ClientOptions {
        control: ControlOptions {
            client_name: "phux-forward".to_owned(),
            attach: None,
            viewport: (0, 0),
            auto_attach_foreign_spawns: false,
            subscribe_agent_sessions: false,
            ..ControlOptions::default()
        },
        ..ClientOptions::default()
    };
    // The runtime reconnects on its own; a forward only has to notice.
    let client = match Runtime::connect(super::upload::managed_target(server.dial()), options) {
        Ok(client) => Arc::new(client),
        Err(err) => {
            error!("{err}");
            return ExitCode::FAILURE;
        }
    };
    let status = rt.block_on(async {
        let activity = Arc::new(Activity(Notify::new()));
        client.set_listener(activity.clone());
        if let Err(code) = attached(&client, &activity).await {
            return code;
        }
        match &forward {
            Some(forward) => serve(&client, &activity, forward).await,
            None => super::forward_agent::serve(&client, &activity, host, yes).await,
        }
    });
    client.close();
    rt.shutdown_background();
    status
}

/// Wait for the first connection to be established.
async fn attached(client: &Client, activity: &Activity) -> Result<(), ExitCode> {
    loop {
        let _ = client.take_events();
        match client.status() {
            Status::Attached => return Ok(()),
            status if status.is_terminal() => return Err(unreachable_server(client)),
            _ => activity.0.notified().await,
        }
    }
}

async fn serve(client: &Arc<Client>, activity: &Activity, spec: &Spec) -> ExitCode {
    if !client.with_control(|control| control.forward_supported()) {
        error!(
            "the server does not advertise FORWARD_TCP: upgrade phux on that host, or set \
             `[forward] tcp = true` in its config"
        );
        return ExitCode::FAILURE;
    }
    let listener = match TcpListener::bind((spec.bind.as_str(), spec.local_port)).await {
        Ok(listener) => listener,
        Err(err) => {
            error!("cannot listen on {}:{}: {err}", spec.bind, spec.local_port);
            return ExitCode::FAILURE;
        }
    };
    let here = listener
        .local_addr()
        .map_or_else(|_| spec.bind.clone(), |addr| addr.to_string());
    outln!(
        "Forwarding {here} to {}:{} on the server; Ctrl-C stops",
        spec.host,
        spec.port
    );
    let mut lost = false;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return ExitCode::SUCCESS,
            accepted = listener.accept() => match accepted {
                Ok((local, _)) => {
                    tokio::spawn(carry(Arc::clone(client), spec.clone(), local));
                }
                Err(err) => warn!("accept on {here}: {err}"),
            },
            () = activity.0.notified() => {
                for event in client.take_events() {
                    match event {
                        // Once per outage: every failed redial reports again.
                        Event::ConnectionLost { .. } if !lost => {
                            lost = true;
                            warn!("lost the phux connection; open connections were closed, reconnecting");
                        }
                        Event::StatusChanged(Status::Attached) if lost => {
                            lost = false;
                            info!("reconnected");
                        }
                        _ => {}
                    }
                }
                if client.status().is_terminal() {
                    return unreachable_server(client);
                }
            }
        }
    }
}

pub(super) fn unreachable_server(client: &Client) -> ExitCode {
    let why = client.last_error().unwrap_or_else(|| "closed".to_owned());
    error!("cannot reach the phux server: {why}");
    ExitCode::FAILURE
}

/// One accepted connection: dial the target, then pump both ways.
async fn carry(client: Arc<Client>, spec: Spec, local: TcpStream) {
    let target = if spec.host.contains(':') {
        format!("[{}]:{}", spec.host, spec.port)
    } else {
        format!("{}:{}", spec.host, spec.port)
    };
    match forward::open(&client, &spec.host, spec.port).await {
        Ok(id) => match forward::relay(&client, id, local).await {
            // A lost connection is reported once, by the accept loop.
            Ok(())
            | Err(
                ForwardError::Disconnected | ForwardError::NotConnected | ForwardError::Local(_),
            ) => {}
            Err(err) => warn!("{target}: {err}"),
        },
        Err(err) => {
            warn!("{target} refused: {err}");
            let _ = local.set_zero_linger();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Spec, parse};

    fn spec(bind: &str, local_port: u16, host: &str, port: u16) -> Spec {
        Spec {
            bind: bind.to_owned(),
            local_port,
            host: host.to_owned(),
            port,
        }
    }

    #[test]
    fn a_forward_names_a_local_port_and_a_remote_host_and_port() {
        assert_eq!(
            parse("3000:localhost:8080"),
            Ok(spec("127.0.0.1", 3000, "localhost", 8080))
        );
        assert_eq!(
            parse("0.0.0.0:3000:127.0.0.1:80"),
            Ok(spec("0.0.0.0", 3000, "127.0.0.1", 80))
        );
        assert_eq!(parse("0:[::1]:80"), Ok(spec("127.0.0.1", 0, "::1", 80)));
        assert_eq!(
            parse("[::1]:3000:db.internal:5432"),
            Ok(spec("::1", 3000, "db.internal", 5432))
        );
        for bad in [
            "",
            "3000",
            "3000:localhost",
            "3000:localhost:",
            "3000::80",
            "x:localhost:80",
            "3000:localhost:65536",
            "a:b:3000:localhost:80",
            "[::1:3000:localhost:80",
            "[::1]3000:localhost:80",
            ":3000:localhost:80",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} parsed");
        }
    }
}
