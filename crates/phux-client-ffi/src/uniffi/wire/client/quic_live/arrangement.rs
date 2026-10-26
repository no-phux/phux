//! The arrangement surface against a live server.

use super::super::organization::arrangement::{
    ArrangementDescriptor, ArrangementEdit, ArrangementNodeDescriptor, WireSplitDir,
};
use super::servers::QuicServer;
use super::*;

/// Spawn a terminal that waits for a line, and return its id once the
/// server reports it.
fn spawn_waiting(remote: &RemoteClient, seen: &mut Vec<WireEvent>) -> String {
    let request_id =
        remote.spawn_terminal_with_command(vec!["/bin/sh".into(), "-c".into(), "read x".into()]);
    assert_ne!(request_id, 0);
    let spawned = |event: &WireEvent| match event {
        WireEvent::TerminalSpawned {
            request_id: id,
            terminal_id,
            error: None,
        } if *id == request_id => terminal_id.clone(),
        _ => None,
    };
    publish_until(remote, seen, |publication| {
        publication
            .events
            .iter()
            .any(|event| spawned(event).is_some())
    });
    seen.iter().find_map(spawned).expect("spawned terminal")
}

/// One session's arrangement as a phone holds it over a live connection.
struct LiveArrangement<'a> {
    remote: &'a RemoteClient,
    key: String,
}

impl<'a> LiveArrangement<'a> {
    /// Read `SESSION`'s arrangement into the replica.
    fn open(remote: &'a RemoteClient) -> Self {
        let session_key = |remote: &RemoteClient| {
            remote.topology().and_then(|topology| {
                topology
                    .sessions
                    .into_iter()
                    .find(|session| session.name == SESSION)
                    .and_then(|session| session.key)
            })
        };
        wait_for(remote, |remote| session_key(remote).is_some());
        let key = session_key(remote).expect("session key");
        remote.fetch_arrangement(key.clone()).expect("valid key");
        let arrangement = Self { remote, key };
        wait_for(remote, |_| arrangement.held().is_some());
        arrangement
    }

    fn held(&self) -> Option<ArrangementDescriptor> {
        self.remote
            .arrangement(self.key.clone())
            .expect("valid key")
    }

    fn read(&self) -> ArrangementDescriptor {
        self.held().expect("held")
    }

    fn try_edit(&self, revision: u64, edit: ArrangementEdit) -> Result<String, WireError> {
        self.remote
            .edit_arrangement(self.key.clone(), revision, edit)
    }

    /// Submit `edit` against `revision`, wait for it to commit, and return
    /// the arrangement once the replica holds a later revision.
    fn commit(&self, revision: u64, edit: ArrangementEdit) -> ArrangementDescriptor {
        let operation = self.try_edit(revision, edit).expect("submitted");
        publish_until(self.remote, &mut Vec::new(), |publication| {
            publication.events.iter().any(|event| {
                matches!(
                    event,
                    WireEvent::OrganizationOperation { operation: id, status, .. }
                        if *id == operation && status == "committed"
                )
            })
        });
        wait_for(self.remote, |_| self.read().revision > revision);
        self.read()
    }
}

/// The mobile arrangement surface against a real server: the session's
/// shared layout reads as a flat tree, and each edit commits through the
/// compare-and-swap and comes back as a new revision.
#[test]
fn a_phone_reads_and_edits_the_shared_arrangement() {
    let server = QuicServer::start();
    let remote = RemoteClient::new(format!("quic://{}", server.addr), 80, 24, None, None);
    remote.connect().expect("connect");
    remote.attach_session(SESSION.to_owned());
    wait_for(&remote, |remote| remote.status() == WireStatus::Attached);
    let added = spawn_waiting(&remote, &mut Vec::new());
    let arrangement = LiveArrangement::open(&remote);

    // The server seeds the session's group with its first pane; a spawned
    // pane is a member but not yet arranged.
    let seeded = arrangement.read();
    let [group] = seeded.groups.as_slice() else {
        panic!("one seeded group: {seeded:?}");
    };
    let [ArrangementNodeDescriptor::Leaf { terminal_id: base }] = group.nodes.as_slice() else {
        panic!("one seeded leaf: {seeded:?}");
    };
    let leaf = |terminal_id: &String| ArrangementNodeDescriptor::Leaf {
        terminal_id: terminal_id.clone(),
    };

    let split = arrangement.commit(
        seeded.revision,
        ArrangementEdit::Split {
            target: base.clone(),
            new_pane: added.clone(),
            dir: WireSplitDir::SideBySide,
            ratio: 0.5,
        },
    );
    assert_eq!(
        split.groups[0].nodes,
        vec![
            ArrangementNodeDescriptor::Split {
                dir: WireSplitDir::SideBySide,
                ratio: 0.5,
                first: 1,
                second: 2,
            },
            leaf(base),
            leaf(&added),
        ]
    );

    let retuned = arrangement.commit(
        split.revision,
        ArrangementEdit::SetRatio {
            group_id: group.id.clone(),
            node: 0,
            ratio: 0.25,
        },
    );
    assert!(matches!(
        retuned.groups[0].nodes[0],
        ArrangementNodeDescriptor::Split { ratio, .. } if (ratio - 0.25).abs() < 1e-6
    ));

    // A gesture made against the old layout never lands on the new one.
    let swap = ArrangementEdit::Swap {
        first: base.clone(),
        second: added.clone(),
    };
    assert!(matches!(
        arrangement.try_edit(split.revision, swap.clone()),
        Err(WireError::Runtime { reason }) if reason == "stale_arrangement"
    ));
    let swapped = arrangement.commit(retuned.revision, swap);
    assert_eq!(swapped.groups[0].nodes[1], leaf(&added));
    remote.stop_connection();
}
