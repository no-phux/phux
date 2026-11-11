//! First-run onboarding's state: which step is showing, what the CLI said
//! about the agent skill and macOS permissions, and the migration it started.
//! The words and parsing live in `crate::onboarding`; the view in
//! `ui/onboarding`.
//!
//! ponytail: the step lives in this app's settings, so another client (the
//! terminal UI, mobile) doesn't know where you got to. Rex keeps it in a
//! server-side onboarding pane so every client agrees; move it there once a
//! second client grows onboarding.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::AttachTarget;
use rune::{Key, KeyEvent, Tx};

use super::{Desk, NoticeKind};

mod fox;
use crate::launch::run_for;
use crate::onboarding::{
    self as words, AgentSkill, Migration, Permissions, Service, Step, unsupported,
};

mod dock;

const DONE_KEY: &str = "onboarding_done";
const STEP_KEY: &str = "onboarding_step";
/// How often the grant sheet asks again, in seconds.
const POLL_EVERY: f64 = 1.0;
const QUICK: Duration = Duration::from_secs(10);
const SLOW: Duration = Duration::from_secs(60);

/// What a CLI call has said so far.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Probe<T> {
    #[default]
    Asking,
    Ready(T),
    /// The installed phux has no such command.
    Unsupported,
    Failed(String),
}

impl<T> Probe<T> {
    fn from(result: Result<T, String>) -> Self {
        match result {
            Ok(value) => Self::Ready(value),
            Err(error) if unsupported(&error) => Self::Unsupported,
            Err(error) => Self::Failed(error),
        }
    }

    pub const fn ready(&self) -> Option<&T> {
        match self {
            Self::Ready(value) => Some(value),
            _ => None,
        }
    }
}

/// The migration, from choosing an agent to its pane.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Migrate {
    #[default]
    Choosing,
    Starting(String),
    /// The agent's pane, beside the step.
    Running(String, Option<ResourceId>),
    Failed(String),
}

enum Done {
    Skills(Result<Vec<AgentSkill>, String>),
    Installed(Result<Vec<AgentSkill>, String>),
    Permissions(Result<Permissions, String>),
    Opened(Result<Option<String>, String>),
    Prompt(Result<String, String>),
    Migrated(String, Result<Migration, String>),
}

pub struct Onboarding {
    pub open: bool,
    pub step: Step,
    pub skills: Probe<Vec<AgentSkill>>,
    pub installing: bool,
    pub permissions: Probe<Permissions>,
    pub grant_sheet: bool,
    /// The server's app, to drag into a privacy list.
    pub bundle: Option<PathBuf>,
    pub migrate: Migrate,
    pub copied: bool,
    /// Keeping the grant sheet clear of System Settings.
    pub dock: dock::Dock,
    /// When the grant sheet last asked, on the app clock, and whether that
    /// answer is still out.
    asked_at: f64,
    asking: bool,
    done: (Sender<Done>, Receiver<Done>),
    pub fox: fox::Fox,
}

impl Default for Onboarding {
    fn default() -> Self {
        Self {
            open: false,
            step: Step::Welcome,
            skills: Probe::Asking,
            installing: false,
            permissions: Probe::Asking,
            grant_sheet: false,
            bundle: None,
            migrate: Migrate::Choosing,
            copied: false,
            dock: dock::Dock::default(),
            asked_at: f64::NEG_INFINITY,
            asking: false,
            done: channel(),
            fox: fox::Fox::default(),
        }
    }
}

impl Onboarding {
    /// Agents that can bring a setup over; none hides that step.
    pub fn migrators(&self) -> Vec<&'static str> {
        self.skills
            .ready()
            .map_or_else(Vec::new, |s| words::migrators(s))
    }

    fn agents(&self) -> bool {
        !self.migrators().is_empty()
    }

    /// The agent's pane is beside the step, and has the keyboard.
    pub fn migrating(&self) -> bool {
        self.open && self.step == Step::Migrate && matches!(self.migrate, Migrate::Running(..))
    }
}

impl Desk {
    /// The first connection of an install that hasn't been through it, picking
    /// up at the step it was left on; else the tour's tip. A run aimed at a
    /// server (development, qualification) or the phone form never opens it
    /// on its own.
    pub(crate) fn first_run(&mut self) {
        let done = self.prefs.get::<bool>(DONE_KEY).unwrap_or(false);
        if done || self.launch.aimed || self.phone.is_some() || self.bare.is_some() {
            return self.offer_tour();
        }
        let step = self.prefs.get::<String>(STEP_KEY);
        self.open_onboarding_at(
            step.as_deref()
                .and_then(Step::from_id)
                .unwrap_or(Step::Welcome),
        );
    }

    /// From the palette or Settings: from the top, whatever was done before.
    pub fn open_onboarding(&mut self) {
        self.open_onboarding_at(Step::Welcome);
    }

    fn open_onboarding_at(&mut self, step: Step) {
        if self.phone.is_some() {
            return;
        }
        self.close_settings();
        self.close_palette();
        self.close_inbox();
        self.onboarding.open = true;
        self.onboarding.migrate = Migrate::Choosing;
        self.onboarding.skills = Probe::Asking;
        self.onboarding.permissions = Probe::Asking;
        self.ask_skills();
        if words::PERMISSIONS {
            self.ask_permissions();
        }
        self.go_to(step);
    }

    fn go_to(&mut self, step: Step) {
        self.onboarding.step = step;
        self.onboarding.fox.since = self.clock;
        self.onboarding.grant_sheet = false;
        self.onboarding.copied = false;
        let _ = self.prefs.set(STEP_KEY, step.id());
    }

    pub fn onboarding_continue(&mut self) {
        let agents = self.onboarding.agents();
        match self.onboarding.step.next(agents) {
            Some(next) => self.go_to(next),
            None => self.start_using(),
        }
    }

    pub fn onboarding_back(&mut self) {
        let agents = self.onboarding.agents();
        if let Some(back) = self.onboarding.step.back(agents) {
            self.go_to(back);
        }
    }

    /// "Skip for now": done, and never offered on its own again.
    pub fn skip_onboarding(&mut self) {
        self.finish_onboarding();
    }

    fn finish_onboarding(&mut self) {
        self.onboarding.open = false;
        self.onboarding.grant_sheet = false;
        let _ = self.prefs.set(DONE_KEY, &true);
        let _ = self.prefs.set(STEP_KEY, Step::Welcome.id());
    }

    /// The last step's button: a normal session, made if there is none.
    pub fn start_using(&mut self) {
        self.finish_onboarding();
        let Some(topology) = &self.topology else {
            return;
        };
        if !topology.sessions.is_empty() {
            return;
        }
        let taken: Vec<String> = topology.sessions.iter().map(|s| s.name.clone()).collect();
        let folder = std::env::var_os("HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        self.create_session(words::session_name(&folder, &taken));
    }

    /// Auto (`None`) follows the system with no theme; Light or Dark keeps
    /// the current theme when it is already that, else takes the first one
    /// that is, in the picker's order.
    pub fn choose_scheme(&mut self, dark: Option<bool>) {
        let Some(dark) = dark else {
            return self.choose_theme(None);
        };
        if self.settings.theme.is_some_and(|t| t.dark == dark) {
            return;
        }
        if let Some(theme) = crate::theme::all().find(|t| t.dark == dark) {
            self.choose_theme(Some(theme));
        }
    }

    /// Enter continues and Escape steps back (skips, on the first step;
    /// closes the grant sheet when it is open). True when taken.
    pub(super) fn onboarding_key(&mut self, event: &KeyEvent) -> bool {
        match event.key {
            Key::Escape if self.onboarding.grant_sheet => self.onboarding.grant_sheet = false,
            Key::Escape if self.onboarding.step == Step::Welcome => self.skip_onboarding(),
            Key::Escape => self.onboarding_back(),
            Key::Enter if !self.onboarding.grant_sheet => self.onboarding_continue(),
            _ => return false,
        }
        true
    }

    /// `phux [--socket S] ARGS` on a worker; `done` turns its output into news.
    fn ask(
        &self,
        args: &[&str],
        timeout: Duration,
        done: impl FnOnce(Result<Vec<u8>, String>) -> Done + Send + 'static,
    ) {
        let sender = self.onboarding.done.0.clone();
        let waker = self.waker.clone();
        let Some(phux) = self.launch.phux.clone() else {
            let _ = sender.send(done(Err("phux is not installed".to_owned())));
            return;
        };
        // ponytail: always this computer's server, even with the window on
        // another machine; a migration started then runs here, out of view.
        // `phux skill` never dials a server and refuses `--socket`.
        let mut argv: Vec<String> = match &self.launch.socket {
            Ok(socket) if args.first() != Some(&"skill") => {
                vec!["--socket".to_owned(), socket.display().to_string()]
            }
            _ => Vec::new(),
        };
        argv.extend(args.iter().map(|a| (*a).to_owned()));
        std::thread::spawn(move || {
            let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
            let _ = sender.send(done(run_for(&phux, &argv, timeout)));
            if let Some(waker) = waker {
                waker();
            }
        });
    }

    fn ask_skills(&self) {
        self.ask(&["skill", "status", "--json"], QUICK, |out| {
            Done::Skills(out.and_then(|b| words::parse_skills(&b)))
        });
    }

    pub fn install_skill(&mut self) {
        if std::mem::replace(&mut self.onboarding.installing, true) {
            return;
        }
        self.ask(&["skill", "setup", "--json"], SLOW, |out| {
            Done::Installed(out.and_then(|b| words::parse_skills(&b)))
        });
    }

    fn ask_permissions(&mut self) {
        self.onboarding.asking = true;
        self.onboarding.asked_at = self.clock;
        self.ask(&["server", "permissions", "--json"], QUICK, |out| {
            Done::Permissions(out.and_then(|b| words::parse_permissions(&b)))
        });
    }

    pub fn open_grant_sheet(&mut self) {
        self.onboarding.grant_sheet = true;
    }

    pub fn close_grant_sheet(&mut self) {
        self.onboarding.grant_sheet = false;
    }

    /// Open `service`'s System Settings pane; the server's app shows beside
    /// the list to drag in, and the sheet moves out from under Settings.
    pub fn grant(&mut self, service: Service) {
        self.start_dock();
        self.ask(
            &["server", "permissions", "--open", service.id(), "--json"],
            QUICK,
            |out| Done::Opened(out.map(|b| words::parse_opened(&b))),
        );
    }

    pub fn copy_migration_prompt(&mut self) {
        self.ask(&["migrate", "--print"], QUICK, |out| {
            Done::Prompt(out.map(|b| String::from_utf8_lossy(&b).into_owned()))
        });
    }

    /// Start `agent` on the migration, in a phux pane shown beside the step.
    pub fn migrate_with(&mut self, agent: &'static str) {
        if matches!(
            self.onboarding.migrate,
            Migrate::Starting(_) | Migrate::Running(..)
        ) {
            return;
        }
        self.onboarding.migrate = Migrate::Starting(agent.to_owned());
        self.ask(&["migrate", "--json", "--agent", agent], SLOW, move |out| {
            Done::Migrated(
                agent.to_owned(),
                out.and_then(|b| words::parse_migration(&b)),
            )
        });
    }

    /// Stop the agent: its pane closes and the tiles come back.
    pub fn cancel_migration(&mut self) {
        if let Migrate::Running(_, Some(pane)) = &self.onboarding.migrate
            && let Some(client) = &self.client
        {
            let _ = client.close_terminals(vec![pane.clone()]);
        }
        self.onboarding.migrate = Migrate::Choosing;
    }

    fn show_migration(&mut self, agent: String, migration: Migration) {
        let pane = migration.pane.map(ResourceId::local);
        match (&pane, migration.session) {
            (Some(pane), Some(session)) => self.show(pane.clone(), &session),
            (None, Some(session)) => {
                if let Some(client) = &self.client {
                    client.attach_session(AttachTarget::ByName(session.clone()));
                }
                self.pending_session = Some(session);
                self.wanted = None;
            }
            (Some(pane), None) => {
                self.want(pane.clone());
            }
            (None, None) => {}
        }
        self.onboarding.migrate = Migrate::Running(agent, pane);
    }

    /// Answers from the CLI, and the grant sheet's next ask. True when
    /// anything on screen changed.
    pub(crate) fn pump_onboarding(&mut self, tx: &mut Tx) -> bool {
        let mut changed = false;
        while let Ok(done) = self.onboarding.done.1.try_recv() {
            changed = true;
            match done {
                Done::Skills(result) => self.onboarding.skills = Probe::from(result),
                Done::Installed(result) => {
                    self.onboarding.installing = false;
                    match Probe::from(result) {
                        Probe::Failed(error) => self.say(NoticeKind::Error, error),
                        probe => self.onboarding.skills = probe,
                    }
                }
                Done::Permissions(result) => {
                    self.onboarding.asking = false;
                    let probe = Probe::from(result);
                    if let Some(path) = probe.ready().and_then(|p| p.responsible_path.clone()) {
                        self.onboarding.bundle.get_or_insert(PathBuf::from(path));
                    }
                    self.onboarding.permissions = probe;
                }
                Done::Opened(Ok(path)) => {
                    if let Some(path) = path {
                        self.onboarding.bundle = Some(PathBuf::from(path));
                    }
                }
                Done::Opened(Err(error)) | Done::Prompt(Err(error)) => {
                    let error = if unsupported(&error) {
                        "Update phux to enable this.".to_owned()
                    } else {
                        error
                    };
                    self.say(NoticeKind::Error, error);
                }
                Done::Prompt(Ok(prompt)) => {
                    tx.copy(prompt);
                    self.onboarding.copied = true;
                }
                Done::Migrated(agent, Ok(migration)) => self.show_migration(agent, migration),
                Done::Migrated(_, Err(error)) => {
                    self.onboarding.migrate = Migrate::Failed(if unsupported(&error) {
                        "Update phux to enable this.".to_owned()
                    } else {
                        error
                    });
                }
            }
        }
        if self.onboarding.open && self.onboarding.grant_sheet {
            let due = self.onboarding.asked_at + POLL_EVERY - self.clock;
            if due <= 0.0 && !self.onboarding.asking {
                self.ask_permissions();
            }
            self.tick_dock(tx);
            tx.wake_after(Duration::from_secs_f64(due.clamp(0.1, POLL_EVERY)));
        }
        changed | self.tick_fox(tx)
    }
}

#[cfg(test)]
mod tests {
    use rune::prefs::Prefs;

    use super::*;
    use crate::launch::Resolved;

    fn desk(prefs: Prefs) -> Desk {
        let mut desk = Desk::new(&Resolved::offline("onboarding"));
        desk.prefs = prefs;
        desk.launch.aimed = false;
        desk
    }

    fn skills(detected: &[&str]) -> Probe<Vec<AgentSkill>> {
        Probe::Ready(
            detected
                .iter()
                .map(|agent| AgentSkill {
                    agent: (*agent).to_owned(),
                    detected: true,
                    ..AgentSkill::default()
                })
                .collect(),
        )
    }

    #[test]
    fn quitting_mid_way_resumes_at_the_step() {
        let dir = std::env::temp_dir().join(format!("phux-rune-onb-{}", std::process::id()));
        let file = dir.join("prefs.json");
        let mut first = desk(Prefs::at(&file));
        first.first_run();
        assert!(first.onboarding.open);
        assert_eq!(first.onboarding.step, Step::Welcome);
        first.onboarding_continue();
        first.onboarding_continue();
        assert_eq!(first.onboarding.step, Step::Agents);
        drop(first);

        let mut again = desk(Prefs::at(&file));
        again.first_run();
        assert!(again.onboarding.open);
        assert_eq!(
            again.onboarding.step,
            Step::Agents,
            "picks up where it was left"
        );
        again.skip_onboarding();
        drop(again);

        let mut after = desk(Prefs::at(&file));
        after.first_run();
        assert!(!after.onboarding.open, "never opens on its own once done");
        assert!(after.notice.is_some(), "the tour's tip instead");
        after.open_onboarding();
        assert_eq!(
            after.onboarding.step,
            Step::Welcome,
            "reopening starts over"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_development_run_never_opens_it_on_its_own() {
        let mut desk = desk(Prefs::memory());
        desk.launch.aimed = true;
        desk.first_run();
        assert!(!desk.onboarding.open);
    }

    #[test]
    fn bringing_your_setup_shows_only_with_an_agent() {
        let mut desk = desk(Prefs::memory());
        desk.open_onboarding();
        desk.go_to(Step::Agents);
        desk.onboarding.skills = skills(&["pi"]);
        desk.onboarding_continue();
        assert_eq!(desk.onboarding.step, Step::Done, "no agent to do it");
        desk.onboarding_back();
        assert_eq!(desk.onboarding.step, Step::Agents);
        desk.onboarding.skills = skills(&["codex"]);
        desk.onboarding_continue();
        assert_eq!(desk.onboarding.step, Step::Migrate);
        assert_eq!(desk.onboarding.migrators(), ["codex"]);
    }

    #[test]
    fn keys_continue_step_back_and_skip() {
        let key = |key| KeyEvent {
            key,
            mods: rune::Mods::default(),
        };
        let mut desk = desk(Prefs::memory());
        desk.open_onboarding();
        assert!(desk.onboarding_key(&key(Key::Enter)));
        assert_eq!(desk.onboarding.step, Step::Look);
        desk.open_grant_sheet();
        assert!(desk.onboarding_key(&key(Key::Escape)));
        assert!(
            !desk.onboarding.grant_sheet,
            "Escape closes the sheet first"
        );
        assert_eq!(desk.onboarding.step, Step::Look);
        assert!(desk.onboarding_key(&key(Key::Escape)));
        assert_eq!(desk.onboarding.step, Step::Welcome);
        assert!(desk.onboarding_key(&key(Key::Escape)));
        assert!(!desk.onboarding.open, "Escape on the first step skips");
    }

    #[test]
    fn a_missing_command_reads_as_update_phux() {
        let probe: Probe<()> = Probe::from(Err("error: unrecognized subcommand 'skill'".into()));
        assert_eq!(probe, Probe::Unsupported);
        let probe: Probe<()> = Probe::from(Err("timed out".into()));
        assert_eq!(probe, Probe::Failed("timed out".into()));
    }
}
