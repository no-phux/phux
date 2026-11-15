//! How onboarding ends: a session, or an agent already at work in one.

use std::path::{Path, PathBuf};

use super::{Done, SLOW};
use crate::desk::Desk;
use crate::onboarding as words;

impl Desk {
    /// The last step's button: a normal session, made if there is none.
    pub fn start_using(&mut self) {
        self.end_onboarding();
        let Some(topology) = &self.connection.topology else {
            return;
        };
        if !topology.sessions.is_empty() {
            return;
        }
        let folder = std::env::var_os("HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        self.create_session(words::session_name(&folder, &[]));
    }

    fn end_onboarding(&mut self) {
        self.finish_onboarding();
        // Onboarding just taught the keys, so no "New here?" tip after it.
        let _ = self.prefs.set(crate::desk::notice::TOUR_OFFERED, &true);
        if words::KEEP_ALIVE && self.onboarding.keep_alive {
            self.ask(&["service", "install", "--adopt"], SLOW, Done::Service);
        }
    }

    /// "Start <agent> here": onboarding ends, and `agent` starts in a session
    /// of its own once the phux skill is in place.
    pub fn start_agent(&mut self, agent: &'static str) {
        let ready = self
            .onboarding
            .skills
            .ready()
            .is_some_and(|s| s.iter().any(|s| s.agent == agent && s.installed()));
        self.end_onboarding();
        self.onboarding.starting = Some(agent.to_owned());
        if ready {
            self.launch_agent();
        } else {
            self.install_skill();
        }
    }

    /// The agent waiting on its skill, now in a pane; the sidebar shows it
    /// working. A failed skill install says so and starts it anyway.
    pub(super) fn launch_agent(&mut self) {
        let Some(agent) = self.onboarding.starting.take() else {
            return;
        };
        let taken: Vec<String> = self
            .connection
            .topology
            .iter()
            .flat_map(|t| t.sessions.iter().map(|s| s.name.clone()))
            .collect();
        let name = words::session_name(Path::new(&agent), &taken);
        self.create_session_running(name.clone(), Some(words::agent_argv(&agent)));
        self.link.keep = Some(name);
    }

    /// No agent here: the keys worth knowing, from the palette.
    pub fn take_the_tour(&mut self) {
        self.start_using();
        self.show_essentials();
    }
}
