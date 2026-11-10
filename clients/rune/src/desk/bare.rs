//! The bare window, an easter egg: one pane with the terminal UI in it and
//! none of the window's chrome.
//!
//! An unlisted chord starts a session of its own whose only pane runs
//! `phux attach` back into the session the window was showing. The terminal
//! UI drives the real session; its pane lives apart, so it never draws
//! itself. Detaching ends the session, and the window goes back where it was.

use phux_client_runtime::Client;
use phux_client_runtime::control::Topology;
use phux_protocol::wire::frame::AttachTarget;

use super::{Desk, NoticeKind};

/// The bare window's session, and the one it attaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bare {
    pub session: String,
    origin: String,
}

impl Bare {
    #[cfg(test)]
    pub(crate) fn test(session: &str, origin: &str) -> Self {
        Self {
            session: session.to_owned(),
            origin: origin.to_owned(),
        }
    }
}

/// Runs on the pane's host, where Rune's own lookup can't reach: a GUI app's
/// `PATH` is minimal, so the usual install locations come first. The pane is
/// this server's own, so attaching to it is nesting by intent.
const SCRIPT: &str = r#"export PHUX_ALLOW_NESTED=1
for p in "$1" "$HOME/.local/bin/phux" /opt/homebrew/bin/phux /usr/local/bin/phux "$HOME/.cargo/bin/phux"; do
  [ -n "$p" ] && [ -x "$p" ] && exec "$p" attach "$2"
done
exec phux attach "$2""#;

/// The pane's argv: the terminal UI attached to `origin`, trying `phux`
/// (this window's CLI, when the pane runs on this computer) first.
pub(super) fn command(phux: Option<&str>, origin: &str) -> Vec<String> {
    let argv = [
        "/bin/sh",
        "-c",
        SCRIPT,
        "phux-bare",
        phux.unwrap_or(""),
        origin,
    ];
    argv.map(str::to_owned).to_vec()
}

/// `stem`, or the first `stem-N` no session has.
fn unused_name(topology: Option<&Topology>, stem: &str) -> String {
    let taken = |name: &str| topology.is_some_and(|t| t.session_named(name).is_some());
    if !taken(stem) {
        return stem.to_owned();
    }
    (2..)
        .map(|n| format!("{stem}-{n}"))
        .find(|name| !taken(name))
        .unwrap_or_else(|| stem.to_owned())
}

impl Desk {
    /// Into the bare window, or back out of it.
    pub fn toggle_bare(&mut self) {
        if self.bare.is_some() {
            self.leave_bare();
        } else {
            self.enter_bare();
        }
    }

    fn enter_bare(&mut self) {
        if self.phone.is_some() || self.client.is_none() {
            return;
        }
        let Some(origin) = self.selected_session_name() else {
            return self.say(NoticeKind::Info, "Open a session first.");
        };
        let session = unused_name(self.topology.as_ref(), "tui");
        // Rune's CLI is only this computer's.
        let local = self.machines.current.is_none();
        let phux = self.launch.phux.as_ref().filter(|_| local);
        let phux = phux.and_then(|path| path.to_str());
        let argv = command(phux, &origin);
        self.create_session_running(session.clone(), Some(argv));
        self.bare = Some(Bare { session, origin });
    }

    /// End the bare session; `settle_bare` takes the window home once it's
    /// gone. While it is still being made there is nothing to end yet, and
    /// forgetting it would land the window in it with its chrome.
    fn leave_bare(&mut self) {
        let Some(bare) = &self.bare else { return };
        if self.pending_session.as_ref() == Some(&bare.session) {
            return;
        }
        let key = self
            .topology
            .as_ref()
            .and_then(|t| t.session_named(&bare.session))
            .and_then(|session| session.key);
        let (Some(client), Some(key)) = (&self.client, key) else {
            self.bare = None;
            return;
        };
        if let Err(error) = client.kill_session(key) {
            self.bare = None;
            self.say(NoticeKind::Error, format!("Session not closed: {error}"));
        }
    }

    /// Whether the bare window just sent the window home. It ends when its
    /// session does, or when the user switches away from it.
    pub(super) fn settle_bare(&mut self, client: &Client) -> bool {
        if self.pending_session.is_some() {
            return false;
        }
        let (Some(bare), Some(topology)) = (&self.bare, &self.topology) else {
            return false;
        };
        let selected = client.selected_session();
        let selected = topology.sessions.iter().find(|s| Some(s.id) == selected);
        if selected.is_some_and(|s| s.name == bare.session) {
            return false;
        }
        let origin = (selected.is_none() && topology.session_named(&bare.origin).is_some())
            .then(|| bare.origin.clone());
        self.bare = None;
        let Some(origin) = origin else { return false };
        client.attach_session(AttachTarget::ByName(origin));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pane_attaches_the_origin_with_nesting_allowed() {
        let argv = command(Some("/x/phux"), "work");
        assert_eq!(&argv[..2], ["/bin/sh", "-c"]);
        assert!(argv[2].starts_with("export PHUX_ALLOW_NESTED=1\n"));
        assert_eq!(&argv[3..], ["phux-bare", "/x/phux", "work"]);
        assert_eq!(command(None, "work")[4], "");
    }

    #[test]
    fn the_script_runs_the_named_cli_with_the_session() {
        let dir = std::env::temp_dir().join(format!("phux-bare-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("phux");
        std::fs::write(&fake, "#!/bin/sh\necho \"$PHUX_ALLOW_NESTED $*\"\n").unwrap();
        std::process::Command::new("chmod")
            .args(["+x", fake.to_str().unwrap()])
            .status()
            .unwrap();
        let argv = command(fake.to_str(), "my session");
        let out = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .output()
            .unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "1 attach my session\n"
        );
    }

    #[test]
    fn names_skip_sessions_that_exist() {
        use phux_client_core::layout::wire::HandleMap;
        use phux_client_runtime::control::SessionDescriptor;
        use phux_protocol::ids::ResourceId;
        let session = |id, name: &str| SessionDescriptor {
            id,
            name: name.to_owned(),
            attached_client_count: 0,
            keep_empty: false,
            created_at_unix_secs: 0,
            key: None,
            lifetime: None,
            revision: None,
            members: 1,
            arrangement: None,
            awaiting_organization: false,
        };
        let topology = Topology {
            sessions: vec![session(1, "tui"), session(2, "tui-2")],
            resources: Vec::new(),
            panes: Vec::new(),
            agent_sessions: Vec::new(),
            documents: Vec::new(),
            satellite_sessions: Vec::new(),
            focused_session: 0,
            focused_pane: ResourceId::local(0),
            authority: None,
            inbox: None,
            handles: HandleMap::new(),
        };
        assert_eq!(unused_name(None, "tui"), "tui");
        assert_eq!(unused_name(Some(&topology), "tui"), "tui-3");
        assert_eq!(unused_name(Some(&topology), "work"), "work");
    }
}
