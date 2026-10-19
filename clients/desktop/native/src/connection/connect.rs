//! Connection construction and passive inventory dialing.
use super::*;

impl Connection {
    /// Dial `socket` and attach `session`. Viewport `(0, 0)` casts no geometry vote.
    pub fn connect(socket: PathBuf, session: String) -> anyhow::Result<Self> {
        Self::connect_with_key(socket, session, None)
    }

    pub(crate) fn connect_with_key(
        socket: PathBuf,
        session: String,
        key: Option<SessionKey>,
    ) -> anyhow::Result<Self> {
        if socket.as_os_str().is_empty() {
            anyhow::bail!("InvalidConnectOptions");
        }
        Self::connect_target(Target::uds(socket), session, key)
    }

    /// Attach `session` on a host from the remote registry.
    pub(crate) fn connect_remote_with_key(
        host: &str,
        session: String,
        key: Option<SessionKey>,
    ) -> anyhow::Result<Self> {
        Self::connect_target(remote_target(host)?, session, key)
    }

    fn connect_target(
        target: Target,
        session: String,
        key: Option<SessionKey>,
    ) -> anyhow::Result<Self> {
        if session.is_empty() || session.len() > 4096 || session.contains('\0') {
            anyhow::bail!("InvalidSessionName");
        }
        let client = open_client(
            target,
            ControlOptions {
                client_name: "phux-desktop".to_owned(),
                viewport: (0, 0),
                attach: key.is_none().then(|| AttachTarget::ByName(session.clone())),
                session_recovery: if key.is_some() {
                    SessionRecovery::CallerValidated
                } else {
                    SessionRecovery::Automatic
                },
                attach_role: Some(RolePolicy::PRIMARY),
                auto_attach_foreign_spawns: false,
                subscribe_agent_sessions: true,
                ..ControlOptions::default()
            },
        )?;
        // A working window projects held approvals; a browsing one does not.
        client.watch_approvals();
        let mut connection = Self::from_client(client, session);
        connection.home_key = key;
        connection.key_bootstrap_pending = key.is_some();
        connection.pinned_home = key.is_some();
        Ok(connection)
    }

    /// Observe inventory without selecting a session, acquiring a view, or spawning work.
    pub(crate) fn browse(socket: &std::path::Path, remote: Option<&str>) -> anyhow::Result<Self> {
        let target = match remote {
            Some(host) => remote_target(host)?,
            None => Target::uds(socket.to_path_buf()),
        };
        let client = open_client(
            target,
            ControlOptions {
                client_name: "phux-desktop-catalog".to_owned(),
                viewport: (0, 0),
                attach: None,
                attach_role: Some(RolePolicy::PRIMARY),
                auto_attach_foreign_spawns: false,
                subscribe_agent_sessions: false,
                ..ControlOptions::default()
            },
        )?;
        let mut connection = Self::from_client(client, String::new());
        connection.browsing = true;
        Ok(connection)
    }

    /// Whether a retained workspace belongs to this exact destination.
    pub(crate) fn home_matches(&self, session: &str, key: Option<SessionKey>) -> bool {
        !self.browsing
            && match key {
                Some(key) => self.home_key == Some(key),
                None => self.home_key.is_none() && self.session == session,
            }
    }

    /// Promote inventory or select another session on the already negotiated socket.
    /// A fresh organization replica must validate keyed destinations before acquisition.
    pub(crate) fn activate_home(
        &mut self,
        session: String,
        key: Option<SessionKey>,
        authority: Option<phux_protocol::ids::AuthorityId>,
    ) -> bool {
        if self.closed || self.status.is_terminal() {
            return false;
        }
        let same = self.home_matches(&session, key);
        self.session = session;
        self.home_key = key;
        self.pinned_home = key.is_some();
        self.expected_authority = authority;
        self.client.with_control(|control| {
            control.set_session_recovery(if key.is_some() {
                SessionRecovery::CallerValidated
            } else {
                SessionRecovery::Automatic
            })
        });
        if !same {
            self.key_bootstrap_pending = key.is_some();
            self.pinned_verified_epoch = None;
        }
        if self.browsing {
            self.browsing = false;
            self.client.with_control(ControlPlane::watch_agent_sessions);
            self.client.watch_approvals();
        }
        if self.pinned_home {
            self.attach_pinned_home();
        } else if !same {
            self.client
                .attach_session(AttachTarget::ByName(self.session.clone()));
        }
        true
    }

    fn from_client(client: Client, session: String) -> Self {
        let (tx, wake_rx) = async_channel::bounded(1);
        client.set_listener(Arc::new(ActivityListener { tx }));
        let status = client.status();
        Self {
            client: Arc::new(client),
            status,
            topology: None,
            server: None,
            error: None,
            agents: HashMap::new(),
            agents_noted: false,
            conversations: HashMap::new(),
            conversation_heads: HashMap::new(),
            projects: HashMap::new(),
            directories: HashMap::new(),
            groups: HashMap::new(),
            approvals: Vec::new(),
            approval_outcomes: Vec::new(),
            session,
            home_key: None,
            key_bootstrap_pending: false,
            pinned_home: false,
            expected_authority: None,
            pinned_verified_epoch: None,
            fetched: HashSet::new(),
            closed: false,
            known: HashSet::new(),
            admitted: HashSet::new(),
            identity: None,
            project_tag: None,
            browsing: false,
            path_answers: Vec::new(),
            file_uploads: Vec::new(),
            wake_rx,
            revision: 0,
        }
    }
}
