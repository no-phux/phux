//! The attached session's arrangement record, read and written over a
//! dedicated control connection on an `ORGANIZATION` server.
//!
//! The attach connection streams pane output, so the correlated helpers in
//! [`phux_client::layout_ops`] run on a second connection: `ARRANGEMENT_GET`,
//! and `ARRANGEMENT_REPLACE` as a compare-and-swap retried on conflict.
//! `ARRANGEMENT_CHANGED` still arrives on the attach connection. A link that
//! cannot connect reports the failure and retries on the next operation; it
//! never falls back to the legacy layout key.

use phux_client::layout_ops::{LayoutOps, LayoutOpsError, ReadArrangement};
use phux_protocol::ResourceId;
use phux_protocol::ids::SessionId;
use phux_protocol::wire::frame::{
    ArrangementGroup, ArrangementOwner, ArrangementValue, EntryTarget, ResourceRecord,
};

use crate::attach::{Dial, connection::Connection};
use crate::layout::ArrangementDoc;

/// Request ids one operation may use before the next starts: a read fetches
/// every organization page and the arrangement.
const REQUEST_ID_STRIDE: u32 = 1024;

/// A dedicated connection editing one session's arrangement record.
pub(in crate::attach) struct ArrangementLink {
    dial: Dial,
    session: SessionId,
    conn: Option<Connection>,
    next_request_id: u32,
    /// Whether a read has succeeded: until one has, the client holds no
    /// copy of the stored arrangement to write over, so it must not write.
    loaded: bool,
    /// Last read's key-space data, from the same cut as the presented layout.
    action_placement: Option<ActionPlacement>,
}

/// Durable evidence from the last arrangement read.
#[derive(Debug)]
pub(in crate::attach) struct ActionPlacement {
    server: Option<Vec<u8>>,
    authority: phux_protocol::ids::AuthorityId,
    stored: ArrangementValue,
    members: Vec<ResourceRecord>,
}

impl ActionPlacement {
    pub(in crate::attach) fn capture(
        &self,
        target: &mut phux_plugin::ActionTarget,
        group: Option<[u8; 16]>,
        focused: Option<&ResourceId>,
    ) {
        target.authority = Some(self.authority.to_string());
        target.arrangement = Some(self.stored.key.to_string());
        match self.stored.owner {
            ArrangementOwner::Session(session) => target.session = Some(session.to_string()),
            ArrangementOwner::Workspace(workspace) => {
                target.workspace = Some(workspace.to_string())
            }
            _ => {}
        }
        let group = self
            .stored
            .document
            .groups
            .iter()
            .find(|row| Some(row.id) == group);
        target.group = group.map(|group| hex_id(&group.id));
        let Some(focused) = focused else { return };
        let record = self
            .members
            .iter()
            .find(|member| member.live.as_ref() == Some(focused));
        target.resource = record.map(|record| record.key.to_string());
        let activation = record.and_then(|record| record.activation);
        target.activation = activation.map(|activation| activation.id.get());
        target.instance = activation.map(|activation| hex_id(activation.instance.as_bytes()));
        target.entry = group.and_then(|group| exact_entry(group, record, focused));
        if target.entry.is_none() {
            // Floating or not-yet-placed focus has no observed entry in this
            // group, even though the layout underneath has an active group.
            target.group = None;
        }
    }
}

fn exact_entry(
    group: &ArrangementGroup,
    record: Option<&ResourceRecord>,
    focused: &ResourceId,
) -> Option<String> {
    let target = match focused {
        ResourceId::Local { .. } => EntryTarget::Member(record?.key),
        ResourceId::Satellite { host, id } => EntryTarget::Satellite {
            host: host.clone(),
            id: *id,
        },
    };
    phux_client_core::layout::wire::unique_entry_for_target(group, &target)
        .map(|entry| hex_id(&entry))
}

fn hex_id(id: &[u8; 16]) -> String {
    use std::fmt::Write as _;
    let mut text = String::with_capacity(32);
    for byte in id {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

impl std::fmt::Debug for ArrangementLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArrangementLink")
            .field("session", &self.session)
            .field("connected", &self.conn.is_some())
            .field("loaded", &self.loaded)
            .finish_non_exhaustive()
    }
}

impl ArrangementLink {
    /// A link for `session` on `dial`. It connects on first use and
    /// reconnects after a transport failure.
    pub(in crate::attach) const fn lazy(dial: Dial, session: SessionId) -> Self {
        Self {
            dial,
            session,
            conn: None,
            next_request_id: 1,
            loaded: false,
            action_placement: None,
        }
    }

    /// Whether a read has succeeded on this link. A read that failed is
    /// "not loaded", never "absent": the stored arrangement may hold groups
    /// this client has not seen, so nothing may be written until it has.
    pub(in crate::attach) const fn loaded(&self) -> bool {
        self.loaded
    }

    /// The session this link edits.
    pub(in crate::attach) const fn session(&self) -> SessionId {
        self.session
    }

    pub(in crate::attach) fn action_placement(
        &self,
        conn: &Connection,
    ) -> Option<&ActionPlacement> {
        // The arrangement door may have redialed a replacement server while
        // the original attach is still draining. Never join their handles.
        let server = conn.server_id()?;
        self.action_placement
            .as_ref()
            .filter(|placement| placement.server.as_deref() == Some(server))
    }

    /// Read the session's arrangement in handle space; `Ok(None)` when the
    /// server lists the session without one.
    ///
    /// # Errors
    ///
    /// Transport, refusal, or translation failures.
    pub(in crate::attach) async fn read(
        &mut self,
    ) -> Result<Option<ArrangementDoc>, LayoutOpsError> {
        let session = self.session;
        let first = self.take_request_ids();
        self.action_placement = None;
        let conn = self.connection().await?;
        let server = conn.server_id().map(<[u8]>::to_vec);
        let result = LayoutOps::new(conn, session, first)
            .read_arrangement()
            .await;
        let read = match result {
            Ok(ReadArrangement {
                document,
                stored,
                authority,
                members,
                ..
            }) => {
                self.action_placement = Some(ActionPlacement {
                    server,
                    authority,
                    stored,
                    members,
                });
                Some(document)
            }
            Err(LayoutOpsError::MissingLayout) => None,
            Err(err) => return Err(self.settle(err)),
        };
        self.loaded = true;
        Ok(read)
    }

    /// Replace the session's arrangement with `document` (compare-and-swap
    /// retried on conflict).
    ///
    /// # Errors
    ///
    /// Transport, refusal, or translation failures.
    pub(in crate::attach) async fn write(
        &mut self,
        document: &ArrangementDoc,
    ) -> Result<(), LayoutOpsError> {
        let session = self.session;
        let first = self.take_request_ids();
        let result = LayoutOps::new(self.connection().await?, session, first)
            .write_and_confirm(document)
            .await;
        result.map(|_| ()).map_err(|err| self.settle(err))
    }

    async fn connection(&mut self) -> Result<&mut Connection, LayoutOpsError> {
        if self.conn.is_none() {
            self.conn = Some(Connection::connect_dial(&self.dial).await?);
        }
        self.conn
            .as_mut()
            .ok_or_else(|| LayoutOpsError::Refused("no control connection".to_owned()))
    }

    /// Drop a connection a transport failure broke; the next operation
    /// reconnects.
    fn settle(&mut self, err: LayoutOpsError) -> LayoutOpsError {
        if matches!(
            err,
            LayoutOpsError::Transport(_)
                | LayoutOpsError::Org(
                    phux_client::org::OrgError::Attach(_)
                        | phux_client::org::OrgError::OutcomeUnknown { .. }
                )
        ) {
            self.conn = None;
        }
        err
    }

    const fn take_request_ids(&mut self) -> u32 {
        let first = self.next_request_id;
        self.next_request_id = first.wrapping_add(REQUEST_ID_STRIDE);
        first
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::unwrap_used,
        clippy::significant_drop_tightening,
        reason = "tests"
    )]

    use phux_client::testkit::{OrgFixture, ScriptSpec, serve_one};
    use phux_protocol::wire::frame::{
        ArrangementDoc as WireDoc, ArrangementGroup, ArrangementNode, EntryRef, EntryTarget,
        SessionLifetime,
    };

    use super::*;
    use crate::layout::{SplitDir, leaves, split_at};

    #[test]
    fn entry_identity_is_missing_for_duplicate_presentations_in_one_group() {
        let resource = ResourceId::satellite("worker", 7);
        let target = EntryTarget::Satellite {
            host: "worker".into(),
            id: 7,
        };
        let mut entry = EntryRef::new([1; 16], target.clone());
        let group = |entry| ArrangementGroup::new([3; 16], None, ArrangementNode::Leaf(entry));
        assert_eq!(
            exact_entry(&group(entry.clone()), None, &resource),
            Some("01".repeat(16))
        );
        entry.tabs.push(EntryRef::new([2; 16], target));
        assert_eq!(exact_entry(&group(entry), None, &resource), None);
    }

    /// The link reads the record in handle space and writes an edit back
    /// whole; the read after the write sees it.
    #[tokio::test]
    async fn reads_and_writes_the_session_arrangement() {
        let mut fixture = OrgFixture::new();
        let work = fixture.session("work", SessionLifetime::Persistent);
        let a = fixture.shell(work);
        let b = fixture.shell(work);
        fixture.arrangement(
            work,
            WireDoc {
                groups: vec![ArrangementGroup {
                    locked: false,
                    id: [7; 16],
                    name: Some("main".to_owned()),
                    tree: ArrangementNode::Leaf(EntryRef::new([1; 16], EntryTarget::Member(a))),
                }],
            },
        );
        let session = fixture.live_session(&work).unwrap();
        let (pane_a, pane_b) = (fixture.live(&a).unwrap(), fixture.live(&b).unwrap());
        let dir = tempfile::tempdir().unwrap();
        let spec = ScriptSpec::new().organization(fixture);
        let (socket, _server) = serve_one(dir.path(), spec);
        let mut link = ArrangementLink::lazy(Dial::uds(&socket), session);
        let mut document = link.read().await.unwrap().expect("an arrangement");
        assert_eq!(document.groups[0].name, "main");
        let tree = document.groups[0].state.tree.clone().unwrap();
        assert_eq!(leaves(&tree), vec![pane_a.clone()]);

        document.groups[0].state.tree =
            Some(split_at(&tree, &pane_a, &pane_b, SplitDir::Vertical, 0.5).unwrap());
        // A satellite leaf is presentation only, and survives the round trip
        // (R21).
        let edge =
            phux_protocol::ResourceId::satellite(phux_protocol::ids::SatelliteHost::new("edge"), 9);
        document.add_group("edge".to_owned(), edge.clone());
        link.write(&document).await.unwrap();
        let read = link.read().await.unwrap().expect("an arrangement");
        assert_eq!(
            leaves(read.groups[0].state.tree.as_ref().unwrap()),
            vec![pane_a, pane_b]
        );
        assert_eq!(
            leaves(read.groups[1].state.tree.as_ref().unwrap()),
            vec![edge]
        );
    }

    /// A revision race that exhausts the compare-and-swap is refused, logged
    /// where the default info filter shows it, and leaves the stored groups
    /// untouched.
    #[tokio::test(flavor = "current_thread")]
    async fn an_exhausted_revision_conflict_is_logged_and_not_committed() {
        let mut fixture = OrgFixture::new();
        let work = fixture.session("work", SessionLifetime::Persistent);
        let pane = fixture.shell(work);
        let extra = fixture.shell(work);
        fixture.arrangement(
            work,
            WireDoc {
                groups: vec![ArrangementGroup {
                    locked: false,
                    id: [7; 16],
                    name: Some("main".to_owned()),
                    tree: ArrangementNode::Leaf(EntryRef::new([1; 16], EntryTarget::Member(pane))),
                }],
            },
        );
        let fixture = fixture.inject_conflicts(phux_client::org::MAX_CAS_ATTEMPTS);
        let session = fixture.live_session(&work).unwrap();
        let extra_live = fixture.live(&extra).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let spec = ScriptSpec::new().organization(fixture);
        let (socket, _server) = serve_one(dir.path(), spec);
        let mut link = ArrangementLink::lazy(Dial::uds(&socket), session);
        let mut document = link.read().await.unwrap().expect("an arrangement");
        document.add_group("extra".to_owned(), extra_live);

        let buf = crate::test_util::LogBuf::default();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_max_level(tracing::Level::INFO)
            .with_writer(buf.clone())
            .finish();
        let _subscriber = tracing::subscriber::set_default(subscriber);
        tracing::callsite::rebuild_interest_cache();
        let err = link.write(&document).await.unwrap_err();
        assert!(
            matches!(err, LayoutOpsError::Conflict),
            "exhausted compare-and-swap: {err:?}"
        );

        let again = link.read().await.unwrap().expect("an arrangement");
        assert_eq!(again.groups.len(), 1);
        assert_eq!(again.groups[0].name, "main");
        let log = buf.text();
        assert!(
            log.contains("arrangement save refused") && log.contains("conflict"),
            "{log}"
        );
    }
}
