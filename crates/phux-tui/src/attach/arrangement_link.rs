//! The attached session's arrangement record, read and written over a
//! dedicated control connection when the server advertises `ORGANIZATION`.
//!
//! The attach connection streams pane output, so the correlated
//! request/response helpers in [`phux_client::layout_ops`] cannot run on it
//! (they would consume interleaved frames). This link owns a second,
//! unsubscribed connection on the attach's dial and drives
//! [`LayoutOps`] over it: `ARRANGEMENT_GET` with key-to-handle translation,
//! and `ARRANGEMENT_REPLACE` as a compare-and-swap retried on conflict.
//! Change notification still arrives on the attach connection as
//! `ARRANGEMENT_CHANGED`, which asks the driver to read again.
//!
//! Against an older server there is no link and the driver keeps the
//! legacy `phux.tui.layout/v1` metadata key.

use phux_client::layout_ops::{LayoutOps, LayoutOpsError};
use phux_protocol::ids::SessionId;

use crate::attach::{AttachError, Dial, connection::Connection};
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
}

impl std::fmt::Debug for ArrangementLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArrangementLink")
            .field("session", &self.session)
            .field("connected", &self.conn.is_some())
            .finish_non_exhaustive()
    }
}

impl ArrangementLink {
    /// Open a link for `session` on `dial`. `Ok(None)` when the server does
    /// not advertise `ORGANIZATION`: the caller keeps the legacy key.
    ///
    /// # Errors
    ///
    /// A failed connect or handshake.
    pub(in crate::attach) async fn open(
        dial: &Dial,
        session: SessionId,
    ) -> Result<Option<Self>, AttachError> {
        let conn = Connection::connect_dial(dial).await?;
        if !phux_client::org::supported(&conn) {
            return Ok(None);
        }
        Ok(Some(Self {
            dial: dial.clone(),
            session,
            conn: Some(conn),
            next_request_id: 1,
        }))
    }

    /// The session this link edits.
    pub(in crate::attach) const fn session(&self) -> SessionId {
        self.session
    }

    /// Read the session's arrangement in handle space; `Ok(None)` when the
    /// session has none yet.
    ///
    /// # Errors
    ///
    /// Transport, refusal, or translation failures.
    pub(in crate::attach) async fn read(
        &mut self,
    ) -> Result<Option<ArrangementDoc>, LayoutOpsError> {
        let session = self.session;
        let first = self.take_request_ids();
        let result = LayoutOps::new(self.connection().await?, session, first)
            .read()
            .await;
        match result {
            Ok(document) => Ok(Some(document)),
            Err(LayoutOpsError::MissingLayout) => Ok(None),
            Err(err) => Err(self.settle(err)),
        }
    }

    /// Replace the session's arrangement with `document`: a compare-and-swap
    /// against the revision just read, retried on conflict, so the write
    /// lands whole. A session with no arrangement yet is seeded.
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
    use phux_protocol::caps::{ServerFeatureExt, ServerFeatureExtSet};
    use phux_protocol::wire::frame::{
        ArrangementDoc as WireDoc, ArrangementGroup, ArrangementNode, EntryRef, EntryTarget,
        SessionLifetime,
    };

    use super::*;
    use crate::layout::{SplitDir, leaves, split_at};

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
                    id: [7; 16],
                    name: Some("main".to_owned()),
                    tree: ArrangementNode::Leaf(EntryRef {
                        entry: [1; 16],
                        target: EntryTarget::Member(a),
                    }),
                }],
            },
        );
        let session = fixture.live_session(&work).unwrap();
        let (pane_a, pane_b) = (fixture.live(&a).unwrap(), fixture.live(&b).unwrap());
        let dir = tempfile::tempdir().unwrap();
        let spec = ScriptSpec::new()
            .server_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::Organization]))
            .organization(fixture);
        let (socket, _server) = serve_one(dir.path(), spec);
        let mut link = ArrangementLink::open(&Dial::uds(&socket), session)
            .await
            .unwrap()
            .expect("the server advertises ORGANIZATION");
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

    /// Without `ORGANIZATION` there is no link: the driver keeps the key.
    #[tokio::test]
    async fn no_link_without_organization() {
        let dir = tempfile::tempdir().unwrap();
        let (socket, _server) = serve_one(dir.path(), ScriptSpec::new());
        let link = ArrangementLink::open(&Dial::uds(&socket), SessionId::new(1))
            .await
            .unwrap();
        assert!(link.is_none());
    }
}
