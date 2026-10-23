//! Inventory: extracted from the shared module.
use super::{
    ArrangementValue, ClientId, CommandResult, CommandValue, OrgFilter, OrganizationPage, Refusal,
    ResourceRecord, ServerState, SessionRecord, Verb, WireResourceId, ow,
};

/// Records per `GET_ORGANIZATION` page when the caller asks for the
/// default.
pub(in crate::runtime) const DEFAULT_PAGE_SIZE: u32 = 100;

/// Most records per page.
pub(in crate::runtime) const MAX_PAGE_SIZE: u32 = 1000;

/// `ARRANGEMENT_GET`.
pub(super) fn arrangement_get(
    s: &ServerState,
    key: &phux_protocol::ids::ArrangementKey,
) -> CommandResult {
    let Some(record) = s.org().arrangement(&ow::core_arrangement_key(key)) else {
        return CommandResult::Refused(ow::refusal(&Refusal::NotFound));
    };
    let Some(owner) = ow::owner(record.owner) else {
        return CommandResult::Refused(ow::refusal(&Refusal::NotFound));
    };
    CommandResult::OkWith(CommandValue::Arrangement(Box::new(ArrangementValue::new(
        *key,
        owner,
        ow::revision(record.revision),
        ow::document(&record.groups),
    ))))
}

/// One `GET_ORGANIZATION` page: the sessions the filter selects and the
/// caller may inventory, in session-key order, each followed by its members
/// in resource-key order. `cursor` is the key of the last record the previous
/// page emitted; this page resumes strictly after that key. Totals count
/// every matching record the caller may inventory; the Intake designation
/// needs Global.
pub(super) fn organization_page(
    s: &mut ServerState,
    client: ClientId,
    filter: OrgFilter,
    page_size: u32,
    cursor: Option<&[u8]>,
) -> CommandResult {
    if let OrgFilter::Resource(key) = filter {
        return resource_page(s, client, key);
    }
    let intake = s.org().authority().intake_session();
    let selected = match selected_sessions(s, filter) {
        Ok(keys) => keys,
        Err(refusal) => return CommandResult::Refused(ow::refusal(&refusal)),
    };
    let visible = visible_sessions(s, client, selected);
    // A page bounds records, not sessions: each session is followed by its
    // admitted members, so a session with thousands of members spans pages.
    // The cursor names the last emitted record. The next page starts strictly
    // after that key, which stays put when a concurrent insert or delete
    // shifts the sequence.
    let size = match page_size {
        0 => DEFAULT_PAGE_SIZE,
        n => n.min(MAX_PAGE_SIZE),
    } as usize;
    let total_sessions = visible.len();
    let total_intake = visible
        .iter()
        .filter(|(key, _)| {
            s.org()
                .session(key)
                .is_some_and(|r| r.awaiting_organization)
        })
        .count();
    let entries = page_entries(s, client, &visible);
    let total_resources = entries.len() - total_sessions;
    debug_assert!(entries.is_sorted_by_key(|entry| entry_pos(*entry)));
    let after = cursor.and_then(page_cursor);
    let start = after.map_or(0, |bound| {
        entries.partition_point(|entry| entry_pos(*entry) <= bound)
    });
    let page: Vec<PageEntry> = entries.iter().skip(start).take(size).copied().collect();
    let next_cursor = if start + page.len() < entries.len() {
        page.last().map(|entry| cursor_bytes(entry_pos(*entry)))
    } else {
        None
    };
    let (sessions, resources) = project_page(s, page);
    let authority = phux_protocol::ids::AuthorityId::new(*s.org().authority().id.as_bytes());
    let Some(authority) = authority else {
        return CommandResult::Refused(ow::refusal(&Refusal::NotFound));
    };
    let head = s.journal_head();
    let page = OrganizationPage::new(authority, phux_protocol::ids::Revision::new(head), head)
        .with_intake(
            crate::policy::filter::admits_global(s, client, Verb::Inventory)
                .then(|| ow::session_key(intake))
                .flatten(),
        )
        .with_sessions(sessions)
        .with_resources(resources)
        .with_totals(
            u32::try_from(total_sessions).unwrap_or(u32::MAX),
            u32::try_from(total_resources).unwrap_or(u32::MAX),
            u32::try_from(total_intake).unwrap_or(u32::MAX),
        )
        .with_next_cursor(next_cursor);
    CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(page)))
}

/// Exact-key reads need Inventory on the resource, not a live handle or
/// authority over unrelated members. Hidden and missing records look alike.
fn resource_page(
    s: &mut ServerState,
    client: ClientId,
    key: phux_protocol::ids::ResourceKey,
) -> CommandResult {
    let resource = s
        .registry()
        .resource_id(&ow::core_resource_key(&key))
        .filter(|id| crate::policy::filter::admits_resource(s, client, Verb::Inventory, *id))
        .and_then(|id| resource_record(s, id));
    let Some(authority) = phux_protocol::ids::AuthorityId::new(*s.org().authority().id.as_bytes())
    else {
        return CommandResult::Refused(ow::refusal(&Refusal::NotFound));
    };
    let count = u32::from(resource.is_some());
    let head = s.journal_head();
    let page = OrganizationPage::new(authority, phux_protocol::ids::Revision::new(head), head)
        .with_resources(resource.into_iter().collect())
        .with_totals(0, count, 0);
    CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(page)))
}

type SessionHandle = (phux_core::SessionKey, phux_core::ids::SessionId);

fn selected_sessions(
    s: &ServerState,
    filter: OrgFilter,
) -> Result<Vec<phux_core::SessionKey>, Refusal> {
    let intake = s.org().authority().intake_session();
    match filter {
        OrgFilter::All => Ok(s.org().sessions().map(|r| r.key).collect()),
        OrgFilter::Session(key) => Ok(vec![ow::core_session_key(&key)]),
        OrgFilter::Resource(key) => Ok(s
            .org()
            .resource(&ow::core_resource_key(&key))
            .map(|r| r.session)
            .into_iter()
            .collect()),
        OrgFilter::Intake => Ok(s
            .org()
            .sessions()
            .filter(|r| r.key == intake || r.awaiting_organization)
            .map(|r| r.key)
            .collect()),
        _ => Err(Refusal::Unsupported {
            capability: "workspace".to_owned(),
        }),
    }
}

fn visible_sessions(
    s: &mut ServerState,
    client: ClientId,
    keys: Vec<phux_core::SessionKey>,
) -> Vec<SessionHandle> {
    let ids: Vec<_> = keys
        .into_iter()
        .filter_map(|key| Some((key, s.registry().session_id(&key)?)))
        .collect();
    for (_, id) in &ids {
        let _ = s.idspace.intern_session(*id);
    }
    ids.into_iter()
        .filter(|(_, id)| crate::policy::filter::admits_session(s, client, Verb::Inventory, *id))
        .collect()
}

fn page_entries(s: &ServerState, client: ClientId, sessions: &[SessionHandle]) -> Vec<PageEntry> {
    let mut entries = Vec::new();
    for (key, id) in sessions {
        entries.push(PageEntry::Session(*key, *id));
        let candidates = s.registry().members(*id);
        let mut members: Vec<_> = candidates
            .into_iter()
            .filter(|member| {
                crate::policy::filter::admits_resource(s, client, Verb::Inventory, *member)
            })
            .filter_map(|member| {
                s.registry()
                    .record(member)
                    .map(|record| (record.key, member))
            })
            .collect();
        members.sort_by_key(|(resource, _)| *resource);
        entries.extend(
            members
                .into_iter()
                .map(|(resource, member)| PageEntry::Resource(*key, resource, member)),
        );
    }
    entries
}

fn project_page(
    s: &mut ServerState,
    entries: Vec<PageEntry>,
) -> (Vec<SessionRecord>, Vec<ResourceRecord>) {
    let mut sessions = Vec::new();
    let mut resources = Vec::new();
    for entry in entries {
        match entry {
            PageEntry::Session(key, id) => sessions.extend(session_record(s, key, id)),
            PageEntry::Resource(_, _, id) => resources.extend(resource_record(s, id)),
        }
    }
    (sessions, resources)
}

fn session_record(
    s: &ServerState,
    key: phux_core::SessionKey,
    id: phux_core::ids::SessionId,
) -> Option<SessionRecord> {
    let record = s.org().session(&key)?;
    Some(
        SessionRecord::new(
            ow::session_key(key)?,
            record.name.clone(),
            ow::revision(record.revision),
        )
        .with_lifetime(ow::lifetime(record.lifetime))
        .with_awaiting_organization(record.awaiting_organization)
        .with_arrangement(record.arrangement.and_then(ow::arrangement_key))
        .with_attrs(ow::attrs(&record.attrs))
        .with_members(u32::try_from(record.members.len()).unwrap_or(u32::MAX))
        .with_live(s.idspace.session_wire(id)),
    )
}

/// One record of a `GET_ORGANIZATION` page, in key order: each session, then
/// its members by resource key.
#[derive(Debug, Clone, Copy)]
pub(super) enum PageEntry {
    Session(phux_core::SessionKey, phux_core::ids::SessionId),
    Resource(
        phux_core::SessionKey,
        phux_core::ResourceKey,
        phux_core::ids::ResourceId,
    ),
}

/// Where a page record sits in key order. A session sorts before its members.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct PagePos {
    pub(super) session: phux_core::SessionKey,
    pub(super) resource: Option<phux_core::ResourceKey>,
}

pub(super) const fn entry_pos(entry: PageEntry) -> PagePos {
    match entry {
        PageEntry::Session(session, _) => PagePos {
            session,
            resource: None,
        },
        PageEntry::Resource(session, resource, _) => PagePos {
            session,
            resource: Some(resource),
        },
    }
}

/// The opaque cursor: the session key, then the resource key when the last
/// record was a member. Any other shape is not a position.
pub(super) fn page_cursor(bytes: &[u8]) -> Option<PagePos> {
    if bytes.len() != 16 && bytes.len() != 32 {
        return None;
    }
    let session = phux_core::SessionKey::from_bytes(bytes[..16].try_into().ok()?);
    let resource = if bytes.len() == 32 {
        Some(phux_core::ResourceKey::from_bytes(
            bytes[16..32].try_into().ok()?,
        ))
    } else {
        None
    };
    Some(PagePos { session, resource })
}

pub(super) fn cursor_bytes(pos: PagePos) -> Vec<u8> {
    let mut bytes = pos.session.as_bytes().to_vec();
    if let Some(resource) = pos.resource {
        bytes.extend_from_slice(resource.as_bytes());
    }
    bytes
}

/// A resource record on the wire, with its live handle while its engine
/// runs here.
pub(super) fn resource_record(
    s: &mut ServerState,
    id: phux_core::ids::ResourceId,
) -> Option<ResourceRecord> {
    let record = s.registry().record(id)?.clone();
    let live: Option<WireResourceId> = s
        .resource_handle(id)
        .is_some()
        .then(|| s.intern_terminal_wire(id));
    let activation = record
        .activation
        .as_ref()
        .map(|a| ow::activation(a, s.activation_persistence_pending(record.key)));
    Some(
        ResourceRecord::new(
            ow::resource_key(record.key)?,
            ow::session_key(record.session)?,
            ow::revision(record.revision),
        )
        .with_name(record.name.clone())
        .with_kind(crate::resource::wire_kind(record.kind))
        .with_on_exit(ow::exit_policy(record.on_exit))
        .with_activation(activation)
        .with_live(live),
    )
}
