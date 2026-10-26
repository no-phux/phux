use phux_protocol::ids::{GroupId, SessionId};
use phux_protocol::wire::frame::{Command, FrameKind, OrgFilter, Refusal, Scope};
use tracing::{debug, info};

use crate::attach::connection::Connection;
use crate::layout::ArrangementDoc;
use crate::layout::layout_key;
use crate::layout::wire::HandleMap;
use crate::org::{self, OrgError, OrgOutcome};

use super::apply_mutation;
use super::{
    DEFAULT_LAYOUT_GROUP_ID, LayoutMutation, LayoutOpsError, ReadArrangement,
    validate_projection_key,
};
use crate::layout::adopt_unarranged_groups;

/// Layout client over a dedicated [`Connection`], allocating request ids from
/// `first_request_id`. See the module docs for the two paths.
#[derive(Debug)]
pub struct LayoutOps<'a> {
    conn: &'a mut Connection,
    session: SessionId,
    group: GroupId,
    key: String,
    /// A named projection never maps to the session arrangement (R17).
    projection: bool,
    next_request_id: u32,
}

impl<'a> LayoutOps<'a> {
    /// Use the default layout Group and begin allocating at `first_request_id`.
    #[must_use]
    pub fn new(conn: &'a mut Connection, session: SessionId, first_request_id: u32) -> Self {
        Self::in_group(conn, session, DEFAULT_LAYOUT_GROUP_ID, first_request_id)
    }

    /// Use an explicit Group (primarily useful to non-default server setups).
    #[must_use]
    pub fn in_group(
        conn: &'a mut Connection,
        session: SessionId,
        group: GroupId,
        first_request_id: u32,
    ) -> Self {
        Self {
            conn,
            session,
            group,
            key: layout_key(session),
            projection: false,
            next_request_id: first_request_id,
        }
    }

    /// Use an explicit named projection key (`--projection`, ADR-0129)
    /// instead of the default `phux.tui.layout/v1/<session>`.
    ///
    /// # Errors
    ///
    /// Returns [`LayoutOpsError::InvalidProjectionKey`] when `key` is not
    /// `<prefix>.layout/v1/<session-id>` for `session`.
    pub fn with_key(
        conn: &'a mut Connection,
        session: SessionId,
        key: String,
        first_request_id: u32,
    ) -> Result<Self, LayoutOpsError> {
        validate_projection_key(&key, session)?;
        Ok(Self {
            conn,
            session,
            group: DEFAULT_LAYOUT_GROUP_ID,
            key,
            projection: true,
            next_request_id: first_request_id,
        })
    }

    /// Whether this client edits the session's arrangement record rather
    /// than legacy layout metadata.
    #[must_use]
    pub fn uses_arrangement(&self) -> bool {
        !self.projection && org::supported(self.conn)
    }

    /// Read this session's current layout: its arrangement record, or the v3
    /// envelope from an older server (earlier schemas refuse).
    ///
    /// # Errors
    ///
    /// Returns [`LayoutOpsError::MissingLayout`] when the session has none,
    /// plus transport, refusal, or decode errors.
    pub async fn read(&mut self) -> Result<ArrangementDoc, LayoutOpsError> {
        if self.uses_arrangement() {
            return self.read_arrangement().await.map(|read| read.document);
        }
        let value = self.get_value().await?;
        let bytes = value.ok_or(LayoutOpsError::MissingLayout)?;
        ArrangementDoc::decode_cbor(&bytes).map_err(Into::into)
    }

    /// Read this session's arrangement record with its key, revision, and
    /// key translation.
    ///
    /// # Errors
    ///
    /// [`LayoutOpsError::MissingLayout`] when the session has no arrangement
    /// (or is not listed), [`LayoutOpsError::Org`] for a failed request.
    pub async fn read_arrangement(&mut self) -> Result<ReadArrangement, LayoutOpsError> {
        let request_id = self.allocate_request_id();
        let inventory = org::fetch(self.conn, OrgFilter::All, request_id).await?;
        self.next_request_id = self.next_request_id.wrapping_add(64);
        let session = inventory
            .session_by_live(self.session)
            .ok_or(LayoutOpsError::MissingLayout)?;
        let key = session.arrangement.ok_or(LayoutOpsError::MissingLayout)?;
        let handles = HandleMap::from_pairs(
            inventory
                .members(&session.key)
                .map(|record| (record.key, record.live.clone())),
        );
        let request_id = self.allocate_request_id();
        let value = match org::arrangement(self.conn, request_id, key).await {
            Ok(value) => value,
            Err(OrgError::Refused(Refusal::NotFound)) => return Err(LayoutOpsError::MissingLayout),
            Err(err) => return Err(err.into()),
        };
        let (document, binding) = crate::layout::wire::from_protocol(&value.document, &handles);
        Ok(ReadArrangement {
            document,
            key,
            revision: value.revision,
            handles,
            binding,
        })
    }

    /// Replace the arrangement `read` came from with `document`, naming the
    /// revision it was read at. `Ok(None)` when another writer committed
    /// first (`CONFLICT`): read again and redo the edit.
    async fn replace(
        &mut self,
        read: &ReadArrangement,
        document: &ArrangementDoc,
    ) -> Result<Option<ArrangementDoc>, LayoutOpsError> {
        let wire = crate::layout::wire::to_protocol(document, &read.handles, &read.binding)?;
        let command = Command::ArrangementReplace {
            operation: org::mint_operation(),
            arrangement: read.key,
            expected: read.revision,
            document: wire,
        };
        debug!(
            arrangement = %read.key,
            expected = read.revision.get(),
            groups = document.groups.len(),
            path = "arrangement",
            "arrangement save attempt"
        );
        let request_id = self.allocate_request_id();
        match org::request(self.conn, request_id, command).await?.0 {
            OrgOutcome::Committed(_) => {
                debug!(
                    arrangement = %read.key,
                    revision = read.revision.get().saturating_add(1),
                    groups = document.groups.len(),
                    path = "arrangement",
                    "arrangement save committed"
                );
                Ok(Some(document.clone()))
            }
            OrgOutcome::Refused(Refusal::Conflict { current }) => {
                debug!(
                    arrangement = %read.key,
                    expected = read.revision.get(),
                    current = current.get(),
                    "arrangement save conflict; retrying"
                );
                Ok(None)
            }
            OrgOutcome::Refused(refusal) => {
                info!(
                    arrangement = %read.key,
                    expected = read.revision.get(),
                    groups = document.groups.len(),
                    reason = refusal.as_str(),
                    path = "arrangement",
                    "arrangement save refused"
                );
                Err(LayoutOpsError::Refused(crate::org::refusal_sentence(
                    &refusal,
                )))
            }
            OrgOutcome::Error { message, .. } => Err(LayoutOpsError::Refused(message)),
            other => Err(LayoutOpsError::Refused(format!(
                "unexpected reply: {other:?}"
            ))),
        }
    }

    /// Edit the arrangement with `edit`, compare-and-swap, retrying on
    /// conflict against a fresh read.
    async fn edit_arrangement(
        &mut self,
        mut edit: impl FnMut(&mut ArrangementDoc) -> Result<(), LayoutOpsError>,
    ) -> Result<ArrangementDoc, LayoutOpsError> {
        for _ in 0..org::MAX_CAS_ATTEMPTS {
            let read = self.read_arrangement().await?;
            let mut document = read.document.clone();
            edit(&mut document)?;
            if let Some(written) = self.replace(&read, &document).await? {
                return Ok(written);
            }
        }
        log_conflict_exhausted();
        Err(LayoutOpsError::Conflict)
    }

    /// The session's arrangement, seeded from `fallback` by
    /// compare-and-swap when it has no groups yet. An arrangement that has
    /// groups is returned unchanged.
    async fn seed_arrangement(
        &mut self,
        fallback: &ArrangementDoc,
    ) -> Result<ArrangementDoc, LayoutOpsError> {
        for _ in 0..org::MAX_CAS_ATTEMPTS {
            let read = self.read_arrangement().await?;
            if !read.document.groups.is_empty() {
                return Ok(read.document);
            }
            let mut document = read.document.clone();
            adopt_unarranged_groups(&mut document, fallback);
            if let Some(written) = self.replace(&read, &document).await? {
                return Ok(written);
            }
        }
        log_conflict_exhausted();
        Err(LayoutOpsError::Conflict)
    }

    /// Read this session's layout, writing and confirming `fallback` only
    /// when no value exists (create-without-attach seeding). Against an
    /// arrangement record, "no value" is an arrangement with no groups.
    ///
    /// # Errors
    ///
    /// Returns transport, refusal, or envelope codec errors.
    pub async fn read_or_seed(
        &mut self,
        fallback: ArrangementDoc,
    ) -> Result<ArrangementDoc, LayoutOpsError> {
        if self.uses_arrangement() {
            return self.seed_arrangement(&fallback).await;
        }
        match self.read().await {
            Ok(workspace) => Ok(workspace),
            Err(LayoutOpsError::MissingLayout) => self.write_and_confirm(&fallback).await,
            Err(err) => Err(err),
        }
    }

    /// Read, mutate, write, and confirm; losing a race to a concurrent
    /// writer is [`LayoutOpsError::NotConfirmed`], never their value.
    ///
    /// # Errors
    ///
    /// Returns transport/envelope errors or a mutation-specific rejection.
    pub async fn mutate(
        &mut self,
        mutation: LayoutMutation,
    ) -> Result<ArrangementDoc, LayoutOpsError> {
        if self.uses_arrangement() {
            return self
                .edit_arrangement(|document| apply_mutation(document, &mutation))
                .await;
        }
        let mut workspace = self.read().await?;
        apply_mutation(&mut workspace, &mutation)?;
        self.write_and_confirm(&workspace).await
    }

    /// Mutate the stored workspace, or seed it from `fallback` when this
    /// session has no layout metadata yet.
    ///
    /// Against an arrangement record, each `fallback` group none of whose
    /// panes is arranged yet is appended before the mutation applies: an
    /// empty arrangement takes `fallback` whole, and a member that is not
    /// arranged (a seed never placed, or a closed view) gets a new group
    /// rather than failing the edit.
    ///
    /// # Errors
    ///
    /// Returns transport/envelope errors or a mutation-specific rejection.
    pub async fn mutate_or_seed(
        &mut self,
        fallback: ArrangementDoc,
        mutation: LayoutMutation,
    ) -> Result<ArrangementDoc, LayoutOpsError> {
        if self.uses_arrangement() {
            return self
                .edit_arrangement(|document| {
                    adopt_unarranged_groups(document, &fallback);
                    apply_mutation(document, &mutation)
                })
                .await;
        }
        let mut workspace = match self.read().await {
            Ok(workspace) => workspace,
            Err(LayoutOpsError::MissingLayout) => fallback,
            Err(err) => return Err(err),
        };
        apply_mutation(&mut workspace, &mutation)?;
        self.write_and_confirm(&workspace).await
    }

    async fn get_value(&mut self) -> Result<Option<Vec<u8>>, LayoutOpsError> {
        let request_id = self.allocate_request_id();
        let reply = self
            .conn
            .request_metadata(request_id, Scope::Group(self.group), self.key.clone())
            .await?;
        // A dedicated, unsubscribed connection: nothing can interleave.
        reply
            .into_result_ignoring_interleaved()
            .map_err(|refusal| LayoutOpsError::Refused(refusal.message))
    }

    /// Overwrite the stored envelope with exactly `workspace` and confirm it
    /// with a trailing read.
    ///
    /// # Errors
    ///
    /// Transport/envelope errors, or [`LayoutOpsError::MissingLayout`] /
    /// [`LayoutOpsError::NotConfirmed`] when the read-back is not exactly the
    /// write (a concurrent writer, or an oversized value silently dropped).
    pub async fn write_and_confirm(
        &mut self,
        workspace: &ArrangementDoc,
    ) -> Result<ArrangementDoc, LayoutOpsError> {
        if self.uses_arrangement() {
            return self
                .edit_arrangement(|document| {
                    document.clone_from(workspace);
                    Ok(())
                })
                .await;
        }
        debug!(
            groups = workspace.groups.len(),
            path = "metadata",
            "arrangement save attempt"
        );
        let bytes = workspace.encode_cbor()?;
        let set_request_id = self.allocate_request_id();
        self.conn
            .send(&FrameKind::SetMetadata {
                request_id: set_request_id,
                scope: Scope::Group(self.group),
                key: self.key.clone(),
                value: bytes.clone(),
            })
            .await?;
        // SET_METADATA has no reply; only an exact byte match proves this
        // write won (a decodable foreign value must not pass).
        let value = self.get_value().await?;
        let read_back = value.ok_or(LayoutOpsError::MissingLayout)?;
        if read_back != bytes {
            info!(
                groups = workspace.groups.len(),
                reason = "read-back mismatch",
                path = "metadata",
                "arrangement save refused"
            );
            return Err(LayoutOpsError::NotConfirmed);
        }
        debug!(
            groups = workspace.groups.len(),
            path = "metadata",
            "arrangement save committed"
        );
        ArrangementDoc::decode_cbor(&read_back).map_err(Into::into)
    }

    const fn allocate_request_id(&mut self) -> u32 {
        let id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1);
        id
    }
}

/// [`LayoutOps::write_and_confirm`] over a fresh connection (`phux
/// workspace restore`).
///
/// # Errors
///
/// A connect failure, or [`LayoutOps::write_and_confirm`]'s own errors.
pub async fn write_layout_on(
    socket_path: &std::path::Path,
    session: SessionId,
    workspace: &ArrangementDoc,
    first_request_id: u32,
) -> Result<(), LayoutOpsError> {
    let mut conn = Connection::connect(socket_path).await?;
    LayoutOps::new(&mut conn, session, first_request_id)
        .write_and_confirm(workspace)
        .await
        .map(|_workspace| ())
}

/// A compare-and-swap gave up. Retries that still have attempts left stay at
/// debug; exhausting them is the refusal an operator can see at info.
fn log_conflict_exhausted() {
    info!(
        attempts = org::MAX_CAS_ATTEMPTS,
        reason = "conflict",
        path = "arrangement",
        "arrangement save refused"
    );
}
