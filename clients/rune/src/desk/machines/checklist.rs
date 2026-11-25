//! What works here (phux #2404): a machine's own checklist, as its server
//! answers `HOST_CHECKLIST` from its `phux doctor --json`, shown under the
//! machine's row in the machines sheet. The server owns the items and their
//! words; this shows them as they come, so an item a newer server adds
//! shows too. A fix is copied, or run in a new pane on that machine.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use phux_client_runtime::control::{ChecklistError, ControlOptions};
use phux_client_runtime::{Client, ClientOptions, Runtime, Target};
use phux_protocol::wire::frame::{LaunchSpec, RolePolicy, SessionLifetime};
use rune::Tx;
use serde_json::Value;

use crate::desk::{Desk, NoticeKind};

/// How long another machine may take to answer the dial.
const DIAL_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Ok,
    Off,
    /// Not yet, and it heals by itself (a listener waiting to bind).
    Waiting,
    /// Could not be checked, or a word this build doesn't know.
    Unknown,
}

/// One line of a machine's checklist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub id: String,
    pub title: String,
    pub verdict: Verdict,
    pub reason: String,
    /// What to run on that machine, one command per line.
    pub fix: Option<String>,
}

/// `None` while the machine is being asked.
type Answer = Option<Result<Vec<Item>, String>>;

/// The checklist out under one machine's row.
pub struct Checklist {
    /// `None` is this computer.
    pub machine: Option<String>,
    /// What it answered, once [`Desk::pump_checklist`] takes it in.
    pub answer: Answer,
    /// Filled by the worker; a checklist put away drops its worker's answer.
    inbox: Arc<Mutex<Answer>>,
    /// The item whose fix was copied last.
    pub copied: Option<String>,
    /// A fix started on another machine: the session to open there.
    ran: Arc<Mutex<Option<Result<String, String>>>>,
}

/// The items of a `checklist` document.
pub fn parse(doc: &Value) -> Vec<Item> {
    let items = doc["items"].as_array().into_iter().flatten();
    items
        .filter_map(|item| {
            let text = |key: &str| item[key].as_str().map(str::to_owned);
            let fix = text("fix_command").map(|fix| {
                let lines = fix.lines().map(str::trim).filter(|line| !line.is_empty());
                lines.collect::<Vec<_>>().join("\n")
            });
            Some(Item {
                id: text("id")?,
                title: text("title")?,
                verdict: match item["status"].as_str() {
                    Some("ok") => Verdict::Ok,
                    Some("off") => Verdict::Off,
                    Some("waiting") => Verdict::Waiting,
                    _ => Verdict::Unknown,
                },
                reason: text("reason").unwrap_or_default(),
                fix: fix.filter(|fix| !fix.is_empty()),
            })
        })
        .collect()
}

/// What to tell the person when there is no checklist.
pub fn words(error: &ChecklistError) -> String {
    match error {
        ChecklistError::NotConnected => "Not connected. Connect to this machine, then refresh.",
        ChecklistError::Unsupported => {
            "This machine's phux is too old to report this. Update phux on it."
        }
        ChecklistError::Disconnected => "The connection dropped. Refresh to try again.",
        ChecklistError::Refused { message, .. } => return message.clone(),
    }
    .to_owned()
}

/// `base`, or `base-2`, `base-3`, … whichever no session is called yet.
pub fn fresh_name(base: &str, taken: &[String]) -> String {
    let free = |name: &String| !taken.contains(name);
    let mut names = std::iter::once(base.to_owned()).chain((2..).map(|n| format!("{base}-{n}")));
    names.find(free).unwrap_or_default()
}

/// The pane's command: the fix, then a shell, so its output stays readable.
fn running(fix: &str) -> Vec<String> {
    let script = format!("{fix}\nexec \"${{SHELL:-/bin/sh}}\"");
    vec!["/bin/sh".to_owned(), "-c".to_owned(), script]
}

/// A short-lived connection to `target` that attaches to nothing, once its
/// server has listed its sessions.
fn dial(target: Target) -> Result<Client, String> {
    let control = ControlOptions {
        client_name: "phux-rune-checklist".to_owned(),
        viewport: (0, 0),
        attach: None,
        attach_role: Some(RolePolicy::PRIMARY),
        auto_attach_foreign_spawns: false,
        subscribe_agent_sessions: false,
        ..ControlOptions::default()
    };
    let options = ClientOptions {
        control,
        ..ClientOptions::default()
    };
    let client = Runtime::connect(target, options).map_err(|e| format!("Couldn't connect: {e}"))?;
    wait(&client, |client| client.topology().is_some())?;
    Ok(client)
}

/// Poll `client` until `ready`, it fails, or the dial times out.
fn wait(client: &Client, ready: impl Fn(&Client) -> bool) -> Result<(), String> {
    let deadline = Instant::now() + DIAL_TIMEOUT;
    loop {
        if client.status().is_terminal() {
            let why = client
                .last_error()
                .unwrap_or_else(|| "it closed".to_owned());
            return Err(format!("Couldn't connect: {why}"));
        }
        if ready(client) {
            return Ok(());
        }
        if Instant::now() > deadline {
            client.close();
            return Err("No answer. Check that phux is running there, then refresh.".to_owned());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn ask(client: &Client) -> Result<Vec<Item>, String> {
    // ponytail: a server that never answers leaves the row asking; Refresh
    // asks again. Put a deadline on it if that is ever seen.
    let call = client
        .with_control(|c| c.host_checklist())
        .map_err(|e| words(&e))?;
    pollster::block_on(call)
        .map(|doc| parse(&doc))
        .map_err(|e| words(&e))
}

/// Start `command` as a new session on `target`, and name it.
fn start(target: Target, base: &str, command: Vec<String>) -> Result<String, String> {
    let client = dial(target)?;
    let taken: Vec<String> = client
        .topology()
        .into_iter()
        .flat_map(|t| t.sessions)
        .map(|s| s.name)
        .collect();
    let name = fresh_name(base, &taken);
    if !client.with_control(|control| control.organization_supported()) {
        client.close();
        return Err("This peer cannot reconcile exact session creation; update phux on it.".into());
    }
    wait(&client, |client| client.organization().is_live())?;
    let phux_client_runtime::control::WorkflowSubmission::Journaled { operation, .. } = client
        .create_session(
            name.clone(),
            SessionLifetime::Ephemeral,
            Some(Box::new(LaunchSpec::new(command))),
        )
    else {
        client.close();
        return Err("Session creation was not queued.".into());
    };
    let result = Mutex::new(None);
    let settled = wait(&client, |client| {
        for event in client.take_events() {
            if let phux_client_runtime::control::Event::OrganizationOperation {
                operation: id,
                outcome,
            } = event
                && id == operation
            {
                *result.lock().unwrap() = Some(match outcome {
                    phux_client_core::org_ops::OrgOutcome::Committed(receipt)
                        if receipt.session.is_some() =>
                    {
                        Ok(())
                    }
                    other => Err(format!("Session creation did not settle: {other:?}")),
                });
            }
        }
        result.lock().unwrap().is_some()
    });
    client.close();
    settled?;
    result
        .into_inner()
        .unwrap()
        .unwrap_or_else(|| Err("Session creation outcome is unknown.".into()))?;
    Ok(name)
}

impl Desk {
    /// Where to dial `machine` from this window: `None` is this computer.
    fn checklist_target(&self, machine: Option<&str>) -> Result<Target, String> {
        match machine {
            Some(host) => Target::resolve(host, None),
            None => self.launch.socket.clone().map(Target::uds),
        }
    }

    /// Show `machine`'s checklist under its row, or put it away.
    pub fn toggle_checklist(&mut self, machine: Option<String>) {
        if self
            .machines
            .checklist
            .as_ref()
            .is_some_and(|c| c.machine == machine)
        {
            self.machines.checklist = None;
        } else {
            self.ask_checklist(machine);
        }
    }

    /// Ask `machine` what works there: this window's own connection when it
    /// is on that machine, else a short-lived one.
    pub fn ask_checklist(&mut self, machine: Option<String>) {
        let inbox = Arc::new(Mutex::new(None));
        let job: Box<dyn FnOnce() -> Result<Vec<Item>, String> + Send> =
            if machine == self.machines.current {
                let call = match &self.connection.client {
                    Some(client) => client.with_control(|c| c.host_checklist()),
                    None => Err(ChecklistError::NotConnected),
                };
                Box::new(move || {
                    let doc = pollster::block_on(call.map_err(|e| words(&e))?);
                    doc.map(|doc| parse(&doc)).map_err(|e| words(&e))
                })
            } else {
                let target = self.checklist_target(machine.as_deref());
                Box::new(move || ask(&dial(target?)?))
            };
        let (filled, waker) = (inbox.clone(), self.waker.clone());
        std::thread::spawn(move || {
            let result = job();
            if let Ok(mut answer) = filled.lock() {
                *answer = Some(result);
            }
            if let Some(waker) = waker {
                waker();
            }
        });
        self.machines.checklist = Some(Checklist {
            machine,
            answer: None,
            inbox,
            copied: None,
            ran: Arc::new(Mutex::new(None)),
        });
    }

    /// From the palette: the machines sheet, this window's machine's
    /// checklist out.
    pub fn what_works_here(&mut self) {
        self.show_machines();
        self.ask_checklist(self.machines.current.clone());
    }

    fn fix_of(&self, id: &str) -> Option<String> {
        let answer = self
            .machines
            .checklist
            .as_ref()?
            .answer
            .as_ref()?
            .as_ref()
            .ok()?;
        answer.iter().find(|item| item.id == id)?.fix.clone()
    }

    pub fn copy_fix(&mut self, id: &str, tx: &mut Tx) {
        let Some(fix) = self.fix_of(id) else { return };
        tx.copy(fix);
        if let Some(checklist) = &mut self.machines.checklist {
            checklist.copied = Some(id.to_owned());
        }
    }

    /// Run `id`'s fix in a new pane on the checklist's machine. This
    /// window's machine opens it here; another machine starts it there, and
    /// the window moves once it has.
    pub fn run_fix(&mut self, id: &str) {
        let (Some(fix), Some(checklist)) = (self.fix_of(id), &self.machines.checklist) else {
            return;
        };
        let machine = checklist.machine.clone();
        let base = format!("fix-{}", id.replace('_', "-"));
        if machine == self.machines.current {
            let taken = self
                .connection
                .topology
                .iter()
                .flat_map(|t| &t.sessions)
                .map(|s| s.name.clone());
            let name = fresh_name(&base, &taken.collect::<Vec<_>>());
            self.close_machines();
            return self.create_session_running(name, Some(running(&fix)));
        }
        let target = self.checklist_target(machine.as_deref());
        let (ran, waker) = (checklist.ran.clone(), self.waker.clone());
        let host = machine
            .as_deref()
            .unwrap_or(crate::machines::LOCAL)
            .to_owned();
        self.say(NoticeKind::Info, format!("Starting the fix on {host}…"));
        std::thread::spawn(move || {
            let result = target.and_then(|target| start(target, &base, running(&fix)));
            if let Ok(mut ran) = ran.lock() {
                *ran = Some(result);
            }
            if let Some(waker) = waker {
                waker();
            }
        });
    }

    /// Take in the machine's answer, and go to a fix started on another
    /// machine. True when the sheet has something new to show.
    pub(in crate::desk) fn pump_checklist(&mut self) -> bool {
        let Some(checklist) = &mut self.machines.checklist else {
            return false;
        };
        let mut changed = false;
        if checklist.answer.is_none()
            && let Some(answer) = checklist.inbox.lock().ok().and_then(|mut a| a.take())
        {
            checklist.answer = Some(answer);
            changed = true;
        }
        let Some(ran) = checklist.ran.lock().ok().and_then(|mut ran| ran.take()) else {
            return changed;
        };
        let machine = checklist.machine.clone();
        match ran {
            Ok(session) => self.open_on(machine, session),
            Err(why) => self.say(NoticeKind::Error, format!("The fix didn't start: {why}")),
        }
        true
    }
}

#[cfg(test)]
mod tests;
