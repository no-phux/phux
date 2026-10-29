//! Seeding the replica from paged `GET_ORGANIZATION` replies.

use super::{
    ArrangementKey, ArrangementOwner, ArrangementValue, Command, HandleMap, HashSet, ORG_PAGE_SIZE,
    OrgFilter, OrganizationPage, OrganizationReplica, ReplicaAction, ReplicaState, ResourceRecord,
    Seed, SessionRecord,
};

impl OrganizationReplica {
    /// Forget everything: a different server, or one without
    /// `ORGANIZATION`.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Start a seed of `filter`: the replica buffers events until it ends.
    /// Returns the first page request.
    pub fn begin_seed(&mut self, filter: OrgFilter) -> Command {
        self.state = ReplicaState::Seeding;
        self.seed = Some(Seed {
            filter,
            page: None,
            sessions: Vec::new(),
            resources: Vec::new(),
        });
        Command::GetOrganization {
            filter,
            page_size: ORG_PAGE_SIZE,
            cursor: None,
        }
    }

    /// Whether a seed is in flight.
    #[must_use]
    pub const fn seeding(&self) -> bool {
        self.seed.is_some()
    }

    /// Feed one page of the seed. Returns the next page request, or, on the
    /// last page, installs the records and replays buffered events past the
    /// cut.
    pub fn apply_page(&mut self, page: OrganizationPage) -> ReplicaAction {
        let Some(seed) = self.seed.as_mut() else {
            return ReplicaAction::None;
        };
        seed.sessions.extend(page.sessions.iter().cloned());
        seed.resources.extend(page.resources.iter().cloned());
        let next = page.next_cursor.clone();
        if seed.page.is_none() {
            seed.page = Some(page);
        }
        if let Some(cursor) = next {
            return ReplicaAction::NextPage(Command::GetOrganization {
                filter: seed.filter,
                page_size: ORG_PAGE_SIZE,
                cursor: Some(cursor),
            });
        }
        let Some(seed) = self.seed.take() else {
            return ReplicaAction::None;
        };
        let Some(first) = seed.page else {
            return ReplicaAction::None;
        };
        self.install(seed.filter, &first, seed.sessions, seed.resources);
        let buffered = std::mem::take(&mut self.buffered);
        for (seq, event) in buffered {
            // A reseed requested while replaying wins; the rest is moot.
            if matches!(self.apply_event(seq, &event), ReplicaAction::Reseed) {
                return ReplicaAction::Reseed;
            }
        }
        ReplicaAction::Seeded
    }

    /// The seed was refused or failed: drop it. The replica keeps what it
    /// had and reports itself stale.
    pub fn abandon_seed(&mut self) {
        self.seed = None;
        self.buffered.clear();
        self.state = ReplicaState::Stale;
    }

    fn install(
        &mut self,
        filter: OrgFilter,
        first: &OrganizationPage,
        sessions: Vec<SessionRecord>,
        resources: Vec<ResourceRecord>,
    ) {
        self.authority = Some(first.authority);
        if let OrgFilter::Session(key) = filter {
            self.sessions.retain(|s| s.key != key);
            self.sessions.extend(sessions);
            self.forget_members(key);
            self.resources.extend(resources);
        } else {
            self.intake = first.intake();
            self.snapshot_revision = first.snapshot_revision;
            self.journal_seq = first.journal_seq;
            self.sessions = sessions;
            self.resources = resources;
            self.handles = HandleMap::new();
            let known: HashSet<ArrangementKey> =
                self.sessions.iter().filter_map(|s| s.arrangement).collect();
            self.arrangements.retain(|key, _| known.contains(key));
        }
        for resource in &self.resources {
            if let Some(live) = &resource.live {
                self.handles.insert(resource.key, live.clone());
            }
        }
        if self.state != ReplicaState::Stale || matches!(filter, OrgFilter::All) {
            self.state = ReplicaState::Live;
        }
    }

    /// Store an arrangement fetched with `ARRANGEMENT_GET`.
    pub fn apply_arrangement(&mut self, value: ArrangementValue) {
        if let ArrangementOwner::Session(owner) = value.owner
            && let Some(session) = self.sessions.iter_mut().find(|s| s.key == owner)
        {
            session.arrangement = Some(value.key);
        }
        self.arrangements.insert(value.key, value);
    }
}
