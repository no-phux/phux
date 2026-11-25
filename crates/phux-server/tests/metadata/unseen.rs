//! `phux.unseen/v1`: a bell neither of two attached clients has in view
//! reaches both, and one looking (`phux.in-view/v1`) clears it for both.

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{FrameKind, RESOURCE_IN_VIEW_KEY, RESOURCE_UNSEEN_KEY, Scope};
use tokio::net::UnixStream;

use phux_server_testkit::{recv_until, run_local, send_frame};

use crate::common::{attach_pane, connect_as, full_caps, gated_server, subscribe_all};

async fn watch(stream: &mut UnixStream, pane: &ResourceId) {
    let subscribe = FrameKind::SubscribeMetadata {
        scope: Scope::Resource(pane.clone()),
        key: RESOURCE_UNSEEN_KEY.to_owned(),
    };
    send_frame(stream, &subscribe).await;
    subscribe_all(stream, 1).await;
}

/// Reads until the mark becomes `want`.
async fn told(stream: &mut UnixStream, want: &[u8]) {
    recv_until(stream, |_, frame| match frame {
        FrameKind::MetadataChanged { key, value, .. } if key == RESOURCE_UNSEEN_KEY => {
            (value.as_deref() == Some(want)).then_some(())
        }
        _ => None,
    })
    .await;
}

#[test]
fn one_client_looking_clears_the_mark_for_both() {
    run_local(async {
        let (server, release) = gated_server("printf '\\a'; sleep 60");
        let (mut desk, _) = connect_as(&server.socket, "unseen-desk", full_caps()).await;
        let (mut phone, _) = connect_as(&server.socket, "unseen-phone", full_caps()).await;
        let pane = attach_pane(&mut desk, "demo").await;
        attach_pane(&mut phone, "demo").await;
        watch(&mut desk, &pane).await;
        watch(&mut phone, &pane).await;

        std::fs::write(&release, b"go").unwrap();
        told(&mut desk, b"bell").await;
        told(&mut phone, b"bell").await;

        let look = FrameKind::SetMetadata {
            request_id: 2,
            scope: Scope::Resource(pane.clone()),
            key: RESOURCE_IN_VIEW_KEY.to_owned(),
            value: b"true".to_vec(),
        };
        send_frame(&mut phone, &look).await;
        told(&mut desk, b"seen").await;
        told(&mut phone, b"seen").await;
    });
}
