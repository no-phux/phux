//! `phux forward --agent`: serve this machine's ssh-agent to the panes on
//! a phux server (`docs/ssh.md#ssh-agent`). Runs until Ctrl-C.
//!
//! The server sends each pane request as an event;
//! [`AgentServing`] answers it from the local agent, a signature only after
//! the user says yes to it here (or ran with `--yes`).

use std::io::{BufRead as _, IsTerminal as _};
use std::process::ExitCode;
use std::sync::Arc;

use phux_client_runtime::agent_forward::{AgentServing, Consent, Question, local_agent};
use phux_client_runtime::control::forward::{self, AgentRequest, ForwardError};
use phux_client_runtime::control::{Event, Status};
use phux_client_runtime::runtime::Client;
use phux_protocol::wire::frame::ErrorCode;

use super::forward::{Activity, unreachable_server};

/// Ask on stderr, read the answer from stdin.
fn ask_terminal(question: &Question) {
    eprint!(
        "phux: let {} sign with {}? [y/N] ",
        question.asker(None),
        question.key
    );
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    let yes = matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes");
    if !yes {
        info!("refused");
    }
    question.answer(yes);
}

/// The keys `host`'s `[[remote]]` entry limits it to.
fn allowed_keys(host: &str) -> Option<Vec<String>> {
    let target = super::remote_target::parse_target(host).ok()?;
    let entry = super::remote_target::find_entry(&target)?;
    phux_config::loader::load()
        .ok()?
        .remote
        .into_iter()
        .nth(entry.index)?
        .forward_agent_keys
}

async fn offer(client: &Client) -> Result<(), ForwardError> {
    client
        .with_control(phux_client_runtime::control::ControlPlane::agent_offer)?
        .await
}

/// Answer `request` and send the answer back.
async fn handle(
    client: Arc<Client>,
    serving: Arc<AgentServing>,
    host: String,
    request: AgentRequest,
) {
    let id = request.id;
    let (done, answered) = tokio::sync::oneshot::channel();
    serving.handle(request, move |answer| {
        let _ = done.send(answer);
    });
    let Ok(answer) = answered.await else {
        return;
    };
    if let Some(key) = answer.signed_unasked {
        info!("{host}: signed with {key}");
    }
    if let Some(why) = answer.refused {
        warn!("{why}");
    }
    let sent = match client.with_control(|control| control.agent_reply(id, answer.message)) {
        Ok(call) => call.await,
        Err(err) => Err(err),
    };
    if let Err(ForwardError::Refused {
        code: ErrorCode::NotFound,
        ..
    }) = sent
    {
        warn!("{host}: the server had stopped waiting for that answer");
    }
}

pub(super) async fn serve(
    client: &Arc<Client>,
    activity: &Activity,
    host: Option<String>,
    yes: bool,
) -> ExitCode {
    if !client.with_control(|control| control.forward_agent_supported()) {
        error!(
            "the server does not advertise FORWARD_AGENT: upgrade phux on that host, and set \
             `[forward] agent = true` in its config"
        );
        return ExitCode::FAILURE;
    }
    let agent = match local_agent(None) {
        Ok(agent) => agent,
        Err(why) => {
            error!("{why}");
            return ExitCode::FAILURE;
        }
    };
    let (consent, how) = if yes {
        (Consent::Always, "signing without asking")
    } else if std::io::stdin().is_terminal() {
        let ask = Consent::Ask(Box::new(|question| ask_terminal(&question)));
        (ask, "each signature is asked here")
    } else {
        let how = "listing keys only: stdin is not a terminal, so every signature is refused \
                   (--yes signs without asking)";
        (Consent::Never, how)
    };
    let keys = host.as_deref().and_then(allowed_keys);
    let host = host.unwrap_or_else(|| "the local phux server".to_owned());
    let serving = Arc::new(AgentServing::new(agent, host.clone(), keys, consent));
    if let Err(err) = offer(client).await {
        error!("the server refused the offer: {err}");
        return ExitCode::FAILURE;
    }
    outln!("Serving your ssh-agent to panes on {host}; {how}; Ctrl-C stops");
    let mut lost = false;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return ExitCode::SUCCESS,
            () = activity.0.notified() => {}
        }
        for event in client.take_events() {
            match event {
                Event::Frame(_) => {
                    if let Some(request) = forward::agent_request(event) {
                        tokio::spawn(handle(
                            Arc::clone(client),
                            Arc::clone(&serving),
                            host.clone(),
                            request,
                        ));
                    }
                }
                // Once per outage: every failed redial reports again.
                Event::ConnectionLost { .. } if !lost => {
                    lost = true;
                    warn!("lost the phux connection; panes get no agent until it is back");
                }
                // A new connection carries no offer: make it again.
                Event::StatusChanged(Status::Attached) if lost => {
                    lost = false;
                    match offer(client).await {
                        Ok(()) => info!("reconnected; serving again"),
                        Err(err) => {
                            error!("reconnected, but the server refused the offer: {err}");
                            return ExitCode::FAILURE;
                        }
                    }
                }
                _ => {}
            }
        }
        if client.status().is_terminal() {
            return unreachable_server(client);
        }
    }
}
