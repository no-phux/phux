#![allow(clippy::unwrap_used, reason = "tests")]

use phux_protocol::wire::frame::ControlClass;
use phux_tree::{DocumentActor, DocumentResource as _, SnapshotRequest, TypedDocument};

use super::super::*;

const DOC: &[u8] = b"# before the upgrade";

/// An old image with a live pane hosting a typed document that declared
/// `Window` control. Returns the pane's and the document's wire ids and
/// the document's publish channel.
fn old_image(
    s: &mut crate::state::ServerState,
) -> (
    phux_protocol::ids::ResourceId,
    phux_protocol::ids::ResourceId,
    tokio::sync::mpsc::Sender<phux_tree::PublishRequest>,
) {
    let (_, pane) = s.seed_session("main");
    let live = crate::runtime::apps::terminal::test_support::seeded_terminal(20, 5, b"");
    let token = live.token.clone();
    let pane_wire = s
        .spawn_resource_actor(pane, live.handle, token, live.actor.run())
        .unwrap();
    let launch = phux_core::LaunchSpec::Document(phux_core::resource::DocumentLaunch {
        content_type: "text/markdown".to_owned(),
    });
    let core = s
        .create_child(
            pane,
            phux_core::NewResource::child(launch, Some("notes".into())),
            &phux_core::Actor::system(),
        )
        .unwrap();
    let token = tokio_util::sync::CancellationToken::new();
    let bundle = DocumentActor::<TypedDocument>::build(pane, "text/markdown", token.clone());
    let publish = bundle
        .handle
        .document::<TypedDocument>()
        .unwrap()
        .publish
        .clone();
    let doc_wire = s
        .spawn_resource_actor(
            core,
            bundle.handle.with_companion(ControlClass::Window),
            token,
            bundle.actor.run(),
        )
        .unwrap();
    s.set_upgrade_context(
        7,
        PathBuf::from("/nonexistent/phux.sock"),
        RuntimeFlags::default(),
    );
    (pane_wire, doc_wire, publish)
}

/// phux-g12pw: a document under a live pane crosses the upgrade under the
/// same wire id with its record, newest document and control class, and
/// waits for its producer to come back; one under a pane that does not
/// cross is dropped.
#[tokio::test(flavor = "current_thread")]
async fn upgrade_carries_documents_with_their_id_and_newest_document() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(Box::pin(async {
            let state = SharedState::new();
            let (pane_wire, doc_wire, publish) = state.with_mut(old_image);
            let (reply, rx) = tokio::sync::oneshot::channel();
            publish
                .send(phux_tree::PublishRequest {
                    bytes: DOC.into(),
                    reply,
                })
                .await
                .unwrap();
            rx.await.unwrap().unwrap();

            // Every kind's cut, as `prepare_upgrade` takes them.
            let mut cuts = crate::runtime::apps::upgrade::begin(&state).unwrap();
            let context = capture_upgrade_context(&state).unwrap();
            assert!(
                context.tree_identity.documents.is_empty(),
                "never part of the identity"
            );
            for cut in &mut cuts {
                cut.cut(&state, Duration::from_secs(2)).await.unwrap();
            }
            let blob = reassemble_unchanged_tree(
                &state,
                context.listener_fd,
                context.listener_fd,
                &context.tree_identity,
                &cuts,
            )
            .unwrap();
            assert_eq!(blob.documents.len(), 1);
            let carried = &blob.documents[0];
            assert_eq!(Some(carried.wire_id), doc_wire.local_id());
            assert_eq!(Some(carried.parent_wire_id), pane_wire.local_id());
            assert_eq!(carried.control, Some(ControlClass::Window.to_u8()));

            // What crosses the exec is the serialized blob.
            let blob = StateBlob::from_bytes(&blob.to_bytes().unwrap()).unwrap();
            let resumed = SharedState::new();
            resumed
                .with_mut(|s| {
                    crate::runtime::apps::terminal::test_support::rebuild_unwired(s, &blob)
                })
                .expect("rebuild");
            let (doc, snapshot) = resumed.with(|s| {
                let doc = s.resource_from_wire(&doc_wire).expect("same `@N`");
                let handle = s.resource_handle(doc).expect("engine");
                let snapshot = handle.document::<TypedDocument>().unwrap().snapshot.clone();
                assert_eq!(handle.parent, s.resource_from_wire(&pane_wire));
                assert_eq!(s.resource_control(doc), Some(ControlClass::Window));
                assert!(
                    s.resource_producer(doc).is_none(),
                    "no producer until one publishes"
                );
                assert_eq!(
                    s.registry()
                        .record(doc)
                        .and_then(|r| r.name.clone())
                        .as_deref(),
                    Some("notes")
                );
                (doc, snapshot)
            });
            let (reply, rx) = tokio::sync::oneshot::channel();
            snapshot.send(SnapshotRequest { reply }).await.unwrap();
            assert_eq!(rx.await.unwrap().document.as_deref(), Some(DOC));

            // A live handle vouches for the activation, so reconcile keeps it.
            resumed.with_mut(crate::state::ServerState::reconcile);
            assert!(resumed.with(|s| s.registry().resource(doc).is_some()));
        }))
        .await;
}
