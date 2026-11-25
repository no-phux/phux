//! Messages stay in the existing hub keeper; these are views, not a second store.

use std::io::Read as _;
use std::path::Path;
use std::process::{Command, ExitCode};
use std::sync::Arc;

use phux_client::conversation::{Conversation, ConversationRoute};
use phux_work::{AckMail, Address, Client, Mail, OperationId, ReadMail, SendMail, Sent};
use serde::{Deserialize, Serialize};

use super::{ChatTarget, display_text, emit, failure};

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Receipt {
    #[serde(rename = "phux_chat_receipt")]
    version: u8,
    conversation: String,
    welcome: String,
}

pub(super) fn client(chat: &Conversation) -> Result<Client, String> {
    let ConversationRoute::Hub { entry, .. } = &chat.route;
    let link = phux_client::hub_mail::HubLink::from_entry(entry)?;
    Ok(Client::remote(Arc::new(link)))
}

pub(super) fn store_message(
    chat: &Conversation,
    body: String,
    operation_id: OperationId,
) -> Result<Sent, String> {
    store_in_thread(chat, &chat.thread, body, operation_id)
}

pub(super) fn store_welcome(
    chat: &Conversation,
    body: String,
    operation_id: OperationId,
) -> Result<Sent, String> {
    store_in_thread(chat, &hello_thread(chat), body, operation_id)
}

fn store_in_thread(
    chat: &Conversation,
    thread: &str,
    body: String,
    operation_id: OperationId,
) -> Result<Sent, String> {
    client(chat)?
        .send_mail(SendMail {
            from: chat.local_address.clone(),
            to: vec![chat.peer_address.clone()],
            thread: thread.to_owned(),
            body,
            operation_id,
        })
        .map_err(|err| {
            format!(
                "{err}; retry only with the identical text and operation id {}",
                operation_id.to_hex()
            )
        })
}

fn hello_thread(chat: &Conversation) -> String {
    format!("hello-{}", chat.id)
}

fn receipt_thread(chat: &Conversation) -> String {
    format!("ready-{}", chat.id)
}

fn expected_receipt(chat: &Conversation) -> Receipt {
    Receipt {
        version: 1,
        conversation: chat.id.clone(),
        welcome: chat.welcome_id.clone(),
    }
}

fn read_thread(chat: &Conversation, thread: String) -> Result<Vec<Mail>, String> {
    client(chat)?
        .read_mail(ReadMail {
            reader: chat.local_address.clone(),
            thread: Some(thread),
            all: true,
            mark_read: false,
            limit: 0,
        })
        .map_err(|err| err.to_string())
}

pub(super) fn receive_welcome(chat: &Conversation) -> Result<Mail, String> {
    read_thread(chat, hello_thread(chat))?.into_iter().find(|mail| {
        mail.id.to_hex() == chat.welcome_id && from_peer(chat, mail)
    }).ok_or_else(|| {
        "enrolled, but the pinned welcome message is not available from the expected peer; no readiness confirmation was sent".to_owned()
    })
}

pub(super) fn confirm_welcome(chat: &Conversation) -> Result<(), String> {
    let body = serde_json::to_string(&expected_receipt(chat))
        .map_err(|err| format!("could not encode the welcome receipt: {err}"))?;
    let operation =
        OperationId::derive_bytes(format!("chat:{}:welcome-receipt", chat.id).as_bytes());
    store_in_thread(chat, &receipt_thread(chat), body, operation).map(drop)
}

fn from_peer(chat: &Conversation, mail: &Mail) -> bool {
    same_address(&mail.from, &chat.peer_address)
        && mail
            .to
            .iter()
            .any(|to| same_address(to, &chat.local_address))
}

fn same_address(left: &str, right: &str) -> bool {
    match (Address::parse(left), Address::parse(right)) {
        (Ok(left), Ok(right)) => left.key() == right.key(),
        _ => false,
    }
}

pub(super) fn history(chat: &Conversation) -> Result<Vec<Mail>, String> {
    let mut messages = read_thread(chat, hello_thread(chat))?;
    messages.extend(read_thread(chat, chat.thread.clone())?);
    messages.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then(left.id.to_hex().cmp(&right.id.to_hex()))
    });
    Ok(messages)
}

pub(super) fn send(
    target: &ChatTarget,
    message: &str,
    operation: Option<&str>,
    json: bool,
) -> ExitCode {
    let result = target.resolve().and_then(|(_, chat)| {
        let body = read_body(message)?;
        let id = operation
            .map_or_else(OperationId::generate, OperationId::parse)
            .map_err(|err| err.to_string())?;
        store_message(&chat, body, id).map(|sent| (sent, id))
    });
    match result {
        Ok((sent, operation)) => {
            if json {
                emit(&serde_json::json!({
                    "state": "stored_on_hub",
                    "mail": sent.mail,
                    "operation_id": operation.to_hex(),
                    "replayed": sent.idempotent_replay,
                }))
            } else {
                outln!(
                    "Stored on hub: {} (operation {}). Not a read receipt.",
                    sent.mail.id.to_hex(),
                    operation.to_hex()
                );
                ExitCode::SUCCESS
            }
        }
        Err(error) => failure(
            json,
            error,
            "check chat status; preserve the operation id when retrying an uncertain send",
        ),
    }
}

fn read_body(message: &str) -> Result<String, String> {
    if message != "-" {
        return Ok(message.to_owned());
    }
    let mut text = String::new();
    std::io::stdin()
        .take(64 * 1024 + 1)
        .read_to_string(&mut text)
        .map_err(|_| "could not read message text from stdin".to_owned())?;
    if text.len() > 64 * 1024 {
        return Err("the message exceeds 64 KiB".to_owned());
    }
    Ok(text)
}

pub(super) fn read(target: &ChatTarget, json: bool) -> ExitCode {
    match target.resolve().and_then(|(_, chat)| history(&chat)) {
        Ok(mail) if json => emit(&serde_json::json!({"mail": mail, "consumes_delivery": false})),
        Ok(mail) => {
            if mail.is_empty() {
                outln!("No messages yet.");
            }
            for message in &mail {
                show_message(message);
            }
            ExitCode::SUCCESS
        }
        Err(error) => failure(
            json,
            error,
            "check the conversation record and the hub's reachability",
        ),
    }
}

pub(super) fn show_message(mail: &Mail) {
    outln!(
        "{}  {}  [{}]",
        display_text(&mail.from),
        mail.created_at,
        mail.id.to_hex()
    );
    for line in display_text(&mail.body).lines() {
        outln!("  {line}");
    }
}

pub(super) fn status(target: &ChatTarget, json: bool) -> ExitCode {
    match target.resolve().and_then(|(path, chat)| {
        let receipts = read_thread(&chat, receipt_thread(&chat))?;
        let expected = expected_receipt(&chat);
        let confirmed = receipts.iter().any(|mail| {
            from_peer(&chat, mail)
                && serde_json::from_str::<Receipt>(&mail.body)
                    .is_ok_and(|receipt| receipt == expected)
        });
        Ok((path, chat, confirmed))
    }) {
        Ok((path, chat, confirmed)) => {
            if json {
                emit(&serde_json::json!({
                    "record": path,
                    "conversation_id": chat.id,
                    "mailbox_reachable": true,
                    "local_address": chat.local_address,
                    "peer_address": chat.peer_address,
                    "peer_confirmed_welcome": confirmed,
                    "agent_delivery": "unverified",
                    "authority": "mail_and_claims_only",
                }))
            } else {
                outln!(
                    "Mailbox reachable. You: {}. Peer: {}.",
                    chat.local_address,
                    chat.peer_address
                );
                outln!(
                    "{}",
                    if confirmed {
                        "Peer confirmed receiving the welcome message."
                    } else {
                        "No peer welcome confirmation yet. Connectivity alone is not delivery."
                    }
                );
                outln!(
                    "Agent delivery remains unverified; use chat run with an installed harness integration."
                );
                ExitCode::SUCCESS
            }
        }
        Err(error) => failure(
            json,
            error,
            "check the pinned hub and credential; no local mailbox fallback was used",
        ),
    }
}

pub(super) fn ack(target: &ChatTarget, id: &str, json: bool) -> ExitCode {
    let result = target.resolve().and_then(|(_, chat)| {
        let id = serde_json::from_value(serde_json::Value::from(id))
            .map_err(|_| "a mail id is 32 nonzero hex digits".to_owned())?;
        client(&chat)?
            .ack_mail(AckMail {
                reader: chat.local_address,
                id,
            })
            .map_err(|err| err.to_string())
    });
    match result {
        Ok(mail) if json => emit(&serde_json::json!({"mail": mail})),
        Ok(_) => {
            outln!("Acknowledged.");
            ExitCode::SUCCESS
        }
        Err(error) => failure(
            json,
            error,
            "acknowledge only an id addressed to this conversation",
        ),
    }
}

pub(super) fn run_agent(target: &ChatTarget, command: &[String]) -> ExitCode {
    match target.resolve().and_then(|(path, _)| {
        let executable =
            phux_config::instance::running_executable().map_err(|err| err.to_string())?;
        let record = path.canonicalize().map_err(|err| err.to_string())?;
        let program = command
            .first()
            .ok_or_else(|| "pass an agent command after --".to_owned())?;
        Command::new(program)
            .args(&command[1..])
            .env(phux_client::conversation::CONTEXT_ENV, &record)
            .env("PHUX_BIN", &executable)
            .env("PHUX_AGENT_PHUX_BIN", &executable)
            .status()
            .map_err(|err| format!("could not run {program}: {err}"))
    }) {
        Ok(status) => status.code().map_or(ExitCode::FAILURE, |code| {
            ExitCode::from(u8::try_from(code).unwrap_or(1))
        }),
        Err(error) => failure(
            false,
            error,
            "install the agent's phux integration, then pass its command after --",
        ),
    }
}

pub(crate) fn reply_command(record: &Path, verb: &str) -> String {
    let executable = phux_config::instance::running_executable()
        .unwrap_or_else(|_| std::path::PathBuf::from("phux"));
    format!(
        "{} chat {verb} --record {}",
        shell_words::quote(&executable.to_string_lossy()),
        shell_words::quote(&record.to_string_lossy())
    )
}
