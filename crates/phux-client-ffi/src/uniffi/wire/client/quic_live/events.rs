//! Events a live server delivers to the mobile drain: a remote program's
//! clipboard write and a watched Global metadata key.

use super::servers::{QuicServer, mint, test_key};
use super::*;

/// A real remote program's copy must reach the mobile event drain, not only
/// an engine unit fixture. The server and its socket/state are tempdir-local.
#[test]
fn a_remote_programs_osc52_reaches_the_mobile_clipboard_event() {
    let server = QuicServer::start();
    let remote = RemoteClient::new(format!("quic://{}", server.addr), 80, 24, None, None);
    remote.connect().expect("connect");
    remote.attach_session(SESSION.to_owned());
    wait_for(&remote, |remote| remote.status() == WireStatus::Attached);
    // Wait for input so the write is live, not part of the initial bootstrap.
    // The second read keeps the process alive until connection cleanup.
    let request_id = remote.spawn_terminal_with_command(vec![
        "/bin/sh".into(),
        "-c".into(),
        "read trigger; printf '\\033]52;c;cmVtb3RlLWNvcHk=\\007'; read keepalive".into(),
    ]);
    assert_ne!(request_id, 0);
    let mut seen = Vec::new();
    publish_until(&remote, &mut seen, |publication| {
        publication.events.iter().any(|event| {
            matches!(
                event,
                WireEvent::TerminalSpawned { request_id: id, terminal_id: Some(_), error: None }
                    if *id == request_id
            )
        })
    });
    let terminal_id = seen
        .iter()
        .find_map(|event| match event {
            WireEvent::TerminalSpawned {
                request_id: id,
                terminal_id,
                ..
            } if *id == request_id => terminal_id.clone(),
            _ => None,
        })
        .expect("spawned terminal");
    wait_for(&remote, |remote| remote.input_ready(terminal_id.clone()));
    remote.send_focus(terminal_id.clone(), true);
    assert_ne!(remote.apply_line(terminal_id.clone(), "copy".into()), 0);
    publish_until(&remote, &mut seen, |publication| {
        publication.events.iter().any(|event| {
            matches!(
                event,
                WireEvent::ClipboardWrite { terminal_id: id, text }
                    if id == &terminal_id && text == "remote-copy"
            )
        })
    });
    assert_eq!(
        seen.iter()
            .filter(|event| matches!(event, WireEvent::ClipboardWrite { .. }))
            .count(),
        1
    );
    remote.stop_connection();
}

/// The owner (the server's own socket) writes or deletes one Global key and
/// reads it back, so the write has landed before this returns.
fn owner_writes(socket: &std::path::Path, key: &str, value: Option<&[u8]>) {
    use phux_protocol::wire::frame::{FrameKind, Scope};
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let mut owner = phux_server_testkit::wait_for_socket(socket, DEADLINE).await;
        let frame = value.map_or_else(
            || FrameKind::DeleteMetadata {
                request_id: 1,
                scope: Scope::Global,
                key: key.to_owned(),
            },
            |value| FrameKind::SetMetadata {
                request_id: 1,
                scope: Scope::Global,
                key: key.to_owned(),
                value: value.to_vec(),
            },
        );
        phux_server_testkit::send_frame(&mut owner, &frame).await;
        phux_server_testkit::send_frame(
            &mut owner,
            &FrameKind::GetMetadata {
                request_id: 2,
                scope: Scope::Global,
                key: key.to_owned(),
            },
        )
        .await;
        phux_server_testkit::recv_until(&mut owner, |_, frame| {
            matches!(frame, FrameKind::MetadataValue { request_id: 2, .. }).then_some(())
        })
        .await;
    });
}

/// Poll the phone until a watched value equal to `want` arrives.
fn wait_for_global(remote: &RemoteClient, want: &WireGlobalMetadata) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let _ = remote.take_events();
        if remote.take_global_metadata().contains(want) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {want:?}");
        thread::sleep(Duration::from_millis(20));
    }
}

/// A paired phone (the scope `phux pair` enrolls) reads and watches an
/// owner-written Global key such as `phux.usage/v1`: the stored value, a
/// change, and a delete all reach `take_global_metadata`.
#[test]
fn a_paired_phone_reads_and_watches_a_global_metadata_key() {
    const KEY: &str = "phux.usage/v1";
    let (server, paths) = QuicServer::start_paired();
    let socket = server.dir.path().join("phux.sock");
    let leaf = phux_server::transport::tls::cert_fingerprint(&server.cert()).expect("leaf");
    let authority = phux_server::workload::ca_fingerprint(&paths.ca_cert).expect("ca");
    let phone = RemoteClient::new(format!("quic://{}", server.addr), 80, 24, Some(leaf), None);
    phone.set_authority_pin(Some(authority));
    phone
        .enroll_device(mint(&paths), test_key())
        .expect("enroll");
    assert_eq!(phone.watch_global_metadata(KEY.to_owned()), 0, "offline");
    phone.connect().expect("connect");
    phone.attach_session(SESSION.to_owned());
    wait_for(&phone, |remote| remote.status() == WireStatus::Attached);

    owner_writes(&socket, KEY, Some(b"{\"v\":1}"));
    assert_ne!(phone.watch_global_metadata(KEY.to_owned()), 0);
    let value = |bytes: Option<&[u8]>| WireGlobalMetadata {
        key: KEY.to_owned(),
        value: bytes.map(<[u8]>::to_vec),
    };
    wait_for_global(&phone, &value(Some(b"{\"v\":1}")));

    owner_writes(&socket, KEY, Some(b"{\"v\":2}"));
    wait_for_global(&phone, &value(Some(b"{\"v\":2}")));
    owner_writes(&socket, KEY, None);
    wait_for_global(&phone, &value(None));
    phone.stop_connection();
}
