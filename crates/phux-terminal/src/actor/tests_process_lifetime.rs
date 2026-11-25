//! EOF closes input admission without inventing delivery for already handed-off work.

#![allow(
    clippy::future_not_send,
    reason = "PTY process tests drive the LocalSet-only actor"
)]

use crate::actor::{
    EncodedInputRequest, TerminalActor, TerminalResource as _, WriteCompletion, WriteCompletionSink,
};
use portable_pty::CommandBuilder;

#[tokio::test(flavor = "current_thread")]
async fn eof_settles_unwritten_input_but_keeps_writer_handoff_indeterminate() {
    let mut bundle = TerminalActor::new(20, 5).unwrap();
    let (_output, mut writer) = bundle.actor.install_test_pty_channels();
    let (admitted, admitted_result) = WriteCompletionSink::channel();
    bundle
        .actor
        .service_encoded_input(EncodedInputRequest::acknowledged(
            b"first".to_vec(),
            admitted,
        ));
    let (queued, queued_result) = WriteCompletionSink::channel();
    bundle
        .handle
        .terminal()
        .unwrap()
        .ordered_input
        .submit(EncodedInputRequest::acknowledged(
            b"second".to_vec(),
            queued,
        ))
        .unwrap();

    bundle.actor.handle_pty_eof().await;
    assert_eq!(
        queued_result.try_recv().unwrap(),
        WriteCompletion::NotWritten
    );
    assert!(
        admitted_result.try_recv().is_err(),
        "writer handoff is not proof of delivery or nonexecution"
    );
    let handed_off = writer.try_recv().unwrap();
    assert_eq!(handed_off.bytes.as_ref(), b"first");
    drop(handed_off);
    assert_eq!(admitted_result.try_recv().unwrap(), WriteCompletion::Failed);

    let (late, late_result) = WriteCompletionSink::channel();
    bundle
        .actor
        .service_encoded_input(EncodedInputRequest::acknowledged(b"late".to_vec(), late));
    assert_eq!(late_result.try_recv().unwrap(), WriteCompletion::NotWritten);
    assert!(
        writer.try_recv().is_err(),
        "post-EOF input never overtakes the original writer handoff"
    );
    assert!(
        bundle
            .handle
            .terminal()
            .unwrap()
            .ordered_input
            .submit(EncodedInputRequest::legacy(b"new".to_vec()))
            .is_err()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn repeated_eof_preserves_the_real_child_exit_and_retained_process_identity() {
    let mut command = CommandBuilder::new("/bin/sh");
    command.args(["-c", "exit 23"]);
    let mut bundle = TerminalActor::new_with_command(command, 20, 5).unwrap();
    let actor = &mut bundle.actor;
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while actor.process.exit.is_none() {
            let event = actor.process.pty_rx.as_mut().unwrap().recv().await;
            actor.service_pty_event(event).await;
        }
    })
    .await
    .expect("real child EOF");
    let first = actor.process_facet();
    assert_eq!(first.exit.as_ref().unwrap().status, Some(23));
    assert!(first.child.is_some());
    assert!(first.foreground.is_none());
    assert!(first.cwd.is_none());
    actor.handle_pty_eof().await;
    assert_eq!(
        actor.process_facet(),
        first,
        "duplicate EOF must not reobserve an unknown or recycled process"
    );
    actor.process.release_pty_after_exit();
    assert_eq!(
        actor.process_facet(),
        first,
        "releasing the PTY retains the original child identity and exit"
    );
}
