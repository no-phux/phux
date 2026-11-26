//! `ATTACH_RESOURCE` against a pane actor that invalidates the first capture.

use phux_protocol::caps::{BootstrapLimits, ClientCapabilities, EngineCodec};
use phux_protocol::wire::frame::ErrorCode;
use phux_terminal_api::actor::TerminalResource as _;
use phux_terminal_api::native_state::NativeStateError;

use crate::runtime::apps::terminal::capture::attach_resource::AttachResourceSession;
use crate::runtime::apps::terminal::test_support::stub_terminal;

/// A resize under the capture (an attaching client's own, usually) is asked
/// for again, not answered as a refusal the client can do nothing with.
#[tokio::test(flavor = "current_thread")]
async fn a_resize_under_an_attach_capture_is_asked_again() {
    let (handle, mut stub) = stub_terminal();
    let terminal = handle.terminal().expect("terminal").clone();
    let (out_tx, _out_rx) = tokio::sync::mpsc::channel(8);
    let wire = phux_protocol::ids::ResourceId::local(1);
    let session = AttachResourceSession {
        out_tx: &out_tx,
        terminal_id: &wire,
        handle,
        terminal,
        client_id: crate::state::ClientId(1),
        stream_id: crate::runtime::stream_id_from(1),
        client_caps: ClientCapabilities::default(),
        stream_profile: phux_protocol::caps::BootstrapStreamProfile::NativeState {
            codec: EngineCodec::LibghosttySnapshotV1,
        },
        bootstrap_limits: BootstrapLimits::default(),
    };
    let actor = async {
        for refusal in [NativeStateError::Resize, NativeStateError::OutOfMemory] {
            let request = stub.native_bootstrap.recv().await.expect("a capture");
            assert!(request.reply.send(Err(refusal)).is_ok());
        }
    };
    let id = phux_protocol::ids::BootstrapId::new(1).expect("id");
    let capture = async {
        let captured = session.capture_native_bootstrap(id).await;
        captured.map(|_| ()).map_err(|failure| failure.code)
    };
    let ((), captured) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(actor, capture)
    })
    .await
    .expect("the invalidated capture was not asked for again");
    assert_eq!(captured, Err(ErrorCode::CodecUnavailable), "a real refusal");
}
