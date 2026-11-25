//! A small line-oriented chat view. Viewing never takes an agent's mail.

use std::collections::HashSet;
use std::io::{BufRead as _, Read as _, Write as _};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use phux_work::OperationId;

use super::{ChatTarget, failure, messages};

pub(super) fn open(target: &ChatTarget) -> ExitCode {
    let (_, chat) = match target.resolve() {
        Ok(resolved) => resolved,
        Err(err) => {
            return failure(
                false,
                err,
                "use the conversation name created by invite or join",
            );
        }
    };
    let initial = match messages::history(&chat) {
        Ok(mail) => mail,
        Err(err) => return failure(false, err, "check the hub before opening the conversation"),
    };
    let mut seen = HashSet::new();
    for mail in initial {
        seen.insert(mail.id.to_hex());
        messages::show_message(&mail);
    }
    outln!(
        "Conversation with {}. Type a message, or /quit. History is not consumed.",
        chat.peer_address
    );
    let stopped = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stopped);
    let incoming = chat.clone();
    let worker = std::thread::spawn(move || watch(&incoming, &mut seen, &worker_stop));
    let result = input(&chat);
    stopped.store(true, Ordering::Relaxed);
    // The reader has the same finite network budgets as chat read. Joining
    // it prevents a failed connection from leaving an invisible watcher.
    if worker.join().is_err() {
        return failure(
            false,
            "the conversation watcher exited unexpectedly",
            "reopen the conversation",
        );
    }
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => failure(
            false,
            err,
            "reopen the conversation; stored messages remain on the hub",
        ),
    }
}

fn input(chat: &phux_client::conversation::Conversation) -> Result<(), String> {
    let stdin = std::io::stdin();
    loop {
        out!("> ");
        std::io::stdout().flush().map_err(|err| err.to_string())?;
        let mut line = String::new();
        let count = stdin
            .lock()
            .take(64 * 1024 + 1)
            .read_line(&mut line)
            .map_err(|err| err.to_string())?;
        if count == 0 {
            return Ok(());
        }
        if count > 64 * 1024 {
            return Err("the message exceeds 64 KiB".to_owned());
        }
        line.truncate(line.trim_end_matches(['\r', '\n']).len());
        if line.trim() == "/quit" {
            return Ok(());
        }
        if line.trim().is_empty() {
            continue;
        }
        let operation = OperationId::generate().map_err(|err| err.to_string())?;
        match messages::store_message(chat, line, operation) {
            Ok(_) => outln!("Stored on hub."),
            Err(error) => error!("{error}"),
        }
    }
}

fn watch(
    chat: &phux_client::conversation::Conversation,
    seen: &mut HashSet<String>,
    stopped: &AtomicBool,
) {
    while !stopped.load(Ordering::Relaxed) {
        match messages::history(chat) {
            Ok(mail) => {
                let mut current = HashSet::new();
                for message in mail {
                    let id = message.id.to_hex();
                    if !seen.contains(&id) {
                        messages::show_message(&message);
                    }
                    current.insert(id);
                }
                *seen = current;
            }
            Err(err) => {
                error!("Conversation disconnected: {err}. Stored messages remain on the hub.");
                return;
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}
