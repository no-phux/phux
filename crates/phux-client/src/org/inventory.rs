//! Inventory: extracted from the shared module.
use super::{
    ArrangementValue, AuthorityId, Command, CommandValue, Connection, Degradation, ExecState,
    FIRST_REQUEST_ID, KEY_MIN_PREFIX, OrgError, OrgFilter, OrgOutcome, OrganizationPage, Outcome,
    ResourceId, ResourceKey, ResourceKind, ResourceRecord, Revision, SessionId, SessionKey,
    SessionRecord, outcome_error, request, supported,
};

/// Records requested per `GET_ORGANIZATION` page.
pub(super) const PAGE_SIZE: u32 = 512;

/// Most pages one [`fetch`] reads before giving up on a runaway cursor.
pub(super) const MAX_PAGES: usize = 4096;

/// Every record one `GET_ORGANIZATION` filter covers, read across all pages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgInventory {
    /// The authority the records belong to.
    pub authority: AuthorityId,
    /// The designated Intake session (legacy public field name retained).
    pub inbox: Option<SessionKey>,
    /// Session records, in page order.
    pub sessions: Vec<SessionRecord>,
    /// Resource records, in page order.
    pub resources: Vec<ResourceRecord>,
    /// The organization revision of the first page's cut.
    pub snapshot_revision: Revision,
    /// The journal `seq` at the first page's cut.
    pub journal_seq: u64,
    /// Matching sessions awaiting organization.
    pub total_intake: u32,
    /// Hub notices that arrived with the pages.
    pub degradation: Degradation,
}

impl OrgInventory {
    /// The designated Intake session; the public `inbox` field is a legacy name.
    #[must_use]
    pub const fn intake(&self) -> Option<SessionKey> {
        self.inbox
    }

    pub(super) fn from_page(page: &OrganizationPage) -> Self {
        Self {
            authority: page.authority,
            inbox: page.inbox,
            sessions: page.sessions.clone(),
            resources: page.resources.clone(),
            snapshot_revision: page.snapshot_revision,
            journal_seq: page.journal_seq,
            total_intake: page.total_intake,
            degradation: Degradation::default(),
        }
    }

    /// The session record `key` names.
    #[must_use]
    pub fn session(&self, key: &SessionKey) -> Option<&SessionRecord> {
        self.sessions.iter().find(|session| session.key == *key)
    }

    /// The session record named `name`.
    #[must_use]
    pub fn session_named(&self, name: &str) -> Option<&SessionRecord> {
        self.sessions.iter().find(|session| session.name == name)
    }

    /// The session record whose live handle is `id`.
    #[must_use]
    pub fn session_by_live(&self, id: SessionId) -> Option<&SessionRecord> {
        self.sessions
            .iter()
            .find(|session| session.live == Some(id))
    }

    /// The resource record `key` names.
    #[must_use]
    pub fn resource(&self, key: &ResourceKey) -> Option<&ResourceRecord> {
        self.resources.iter().find(|resource| resource.key == *key)
    }

    /// The resource record executing as `id` here.
    #[must_use]
    pub fn resource_by_live(&self, id: &ResourceId) -> Option<&ResourceRecord> {
        self.resources
            .iter()
            .find(|resource| resource.live.as_ref() == Some(id))
    }

    /// The members of `session`, in page order.
    pub fn members<'a>(
        &'a self,
        session: &'a SessionKey,
    ) -> impl Iterator<Item = &'a ResourceRecord> + 'a {
        self.resources
            .iter()
            .filter(move |resource| resource.session == *session)
    }

    /// The one session or resource whose key starts with `prefix`
    /// (`^ab12cd`, at least six base32 digits, case-insensitive).
    ///
    /// # Errors
    ///
    /// [`OrgError::NotFound`] on no match or a too-short prefix,
    /// [`OrgError::Ambiguous`] on several.
    pub fn resolve_key_prefix(&self, prefix: &str) -> Result<KeyMatch, OrgError> {
        let prefix = prefix.strip_prefix('^').unwrap_or(prefix);
        if prefix.len() < KEY_MIN_PREFIX {
            return Err(OrgError::NotFound(format!("^{prefix}")));
        }
        let mut matches: Vec<KeyMatch> = self
            .sessions
            .iter()
            .filter(|session| session.key.has_prefix(prefix))
            .map(|session| KeyMatch::Session(session.clone()))
            .collect();
        matches.extend(
            self.resources
                .iter()
                .filter(|resource| resource.key.has_prefix(prefix))
                .map(|resource| KeyMatch::Resource(resource.clone())),
        );
        match matches.len() {
            0 => Err(OrgError::NotFound(format!("^{prefix}"))),
            1 => Ok(matches.remove(0)),
            _ => Err(OrgError::Ambiguous {
                prefix: format!("^{prefix}"),
                candidates: matches.iter().map(KeyMatch::key_text).collect(),
            }),
        }
    }

    /// Resources whose activation ended because a server stopped while it
    /// ran (outcome unobserved), optionally only in `session`: what
    /// `phux restore` relaunches.
    #[must_use]
    pub fn ended_by_restart(&self, session: Option<&SessionKey>) -> Vec<&ResourceRecord> {
        self.resources
            .iter()
            .filter(|resource| session.is_none_or(|key| resource.session == *key))
            .filter(|resource| {
                resource.activation.is_some_and(|activation| {
                    activation.state == ExecState::Completed
                        && activation.outcome == Some(Outcome::Unobserved)
                })
            })
            .collect()
    }

    /// What `phux restore` relaunches, in record order: every resource a
    /// server restart ended ([`Self::ended_by_restart`]) and every Terminal
    /// never started (a dormant record, such as an imported archive's pane),
    /// optionally only in `session`.
    #[must_use]
    pub fn restorable(&self, session: Option<&SessionKey>) -> Vec<&ResourceRecord> {
        let ended: Vec<ResourceKey> = self
            .ended_by_restart(session)
            .into_iter()
            .map(|record| record.key)
            .collect();
        self.resources
            .iter()
            .filter(|resource| {
                ended.contains(&resource.key)
                    || (resource.activation.is_none()
                        && resource.kind == ResourceKind::Terminal
                        && session.is_none_or(|key| resource.session == *key))
            })
            .collect()
    }
}

/// Display names for the keys one output lists, written `^ab12cd`.
///
/// Each is the key's shortest prefix that no other listed key shares, at
/// least six base32 digits ([`phux_core::Key16::short`]). Session and
/// resource keys share one namespace, as
/// [`OrgInventory::resolve_key_prefix`] resolves both.
#[derive(Debug, Clone, Default)]
pub struct KeyNames {
    pub(super) keys: Vec<phux_core::ResourceKey>,
}

impl KeyNames {
    /// Names unique among `keys` (raw key bytes).
    #[must_use]
    pub fn new(keys: impl IntoIterator<Item = [u8; 16]>) -> Self {
        Self {
            keys: keys
                .into_iter()
                .map(phux_core::ResourceKey::from_bytes)
                .collect(),
        }
    }

    /// Names unique among every session and resource record of `inventory`.
    #[must_use]
    pub fn of_inventory(inventory: &OrgInventory) -> Self {
        Self::new(
            inventory
                .sessions
                .iter()
                .map(|session| *session.key.as_bytes())
                .chain(
                    inventory
                        .resources
                        .iter()
                        .map(|resource| *resource.key.as_bytes()),
                ),
        )
    }

    /// `^` and the shortest unique prefix of `key`'s text.
    #[must_use]
    pub fn display(&self, key: [u8; 16]) -> String {
        use phux_core::Key16 as _;
        format!(
            "^{}",
            phux_core::ResourceKey::from_bytes(key).short(self.keys.iter())
        )
    }
}

/// What a key prefix names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyMatch {
    /// A session record.
    Session(SessionRecord),
    /// A resource record.
    Resource(ResourceRecord),
}

impl KeyMatch {
    /// The full key text.
    #[must_use]
    pub fn key_text(&self) -> String {
        match self {
            Self::Session(session) => session.key.to_string(),
            Self::Resource(resource) => resource.key.to_string(),
        }
    }
}

/// Read every record `filter` covers, across all pages. A cursor the server
/// no longer holds restarts from the first page, once.
///
/// # Errors
///
/// [`request`]'s errors, a refusal, or an unexpected reply.
pub async fn fetch(
    conn: &mut Connection,
    filter: OrgFilter,
    first_request_id: u32,
) -> Result<OrgInventory, OrgError> {
    let mut restarted = false;
    let mut request_id = first_request_id;
    'restart: loop {
        let mut inventory: Option<OrgInventory> = None;
        let mut cursor = None;
        for _ in 0..MAX_PAGES {
            let (outcome, degradation) = request(
                conn,
                request_id,
                Command::GetOrganization {
                    filter,
                    page_size: PAGE_SIZE,
                    cursor: cursor.take(),
                },
            )
            .await?;
            request_id = request_id.wrapping_add(1);
            let page = match outcome {
                OrgOutcome::Value(value) => match *value {
                    CommandValue::OrganizationPage(page) => page,
                    other => return Err(OrgError::Unexpected(format!("{other:?}"))),
                },
                OrgOutcome::Refused(_) if inventory.is_some() && !restarted => {
                    restarted = true;
                    continue 'restart;
                }
                other => return Err(outcome_error(other)),
            };
            let next = page.next_cursor.clone();
            match inventory.as_mut() {
                None => {
                    let mut first = OrgInventory::from_page(&page);
                    first.degradation = degradation;
                    inventory = Some(first);
                }
                Some(inventory) => {
                    inventory.sessions.extend(page.sessions.iter().cloned());
                    inventory.resources.extend(page.resources.iter().cloned());
                }
            }
            match next {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        return inventory.ok_or_else(|| OrgError::Unexpected("no page".to_owned()));
    }
}

/// Read an authority's organization records from a socket. `None` means the
/// server predates `ORGANIZATION`; a supported server's read errors propagate.
///
/// # Errors
///
/// Connection or organization-read errors.
pub async fn fetch_from_socket(
    socket_path: &std::path::Path,
) -> Result<Option<OrgInventory>, OrgError> {
    let mut conn = Connection::connect(socket_path)
        .await
        .map_err(OrgError::Attach)?;
    if !supported(&conn) {
        return Ok(None);
    }
    fetch(&mut conn, OrgFilter::All, FIRST_REQUEST_ID)
        .await
        .map(Some)
}

/// Read one arrangement document and its revision.
///
/// # Errors
///
/// As [`request`], plus a refusal or an unexpected reply.
pub async fn arrangement(
    conn: &mut Connection,
    request_id: u32,
    key: phux_protocol::ids::ArrangementKey,
) -> Result<ArrangementValue, OrgError> {
    match request(
        conn,
        request_id,
        Command::ArrangementGet { arrangement: key },
    )
    .await?
    .0
    {
        OrgOutcome::Value(value) => match *value {
            CommandValue::Arrangement(arrangement) => Ok(*arrangement),
            other => Err(OrgError::Unexpected(format!("{other:?}"))),
        },
        other => Err(outcome_error(other)),
    }
}
