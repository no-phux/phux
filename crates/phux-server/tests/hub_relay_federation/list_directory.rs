//! `LIST_DIRECTORY.host` through the real UDS -> hub -> WebSocket -> satellite
//! path (`docs/spec/L3.md` §4.1): a relayed listing, an unknown host, a host
//! sent to a server that is not a hub, an unreachable satellite, and a
//! satellite that completes its handshake and then never answers.

use super::*;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, ColorSupport, LayerSet, ServerCapabilities, ServerFeature,
    ServerFeatureSet,
};
use phux_protocol::wire::frame::{
    DirectoryErrorCode, DirectoryListingError, DirectoryListingResult,
};

/// Connect as an L3 consumer (the query is L3-gated, §1.2) and require the
/// host-aware listing bit, which every current server advertises.
async fn connect_l3(path: &std::path::Path) -> UnixStream {
    let mut stream = wait_for_raw_socket(path, STEP_DEADLINE).await;
    send_frame(
        &mut stream,
        &FrameKind::Hello {
            client_name: "hub-list-directory-test".to_owned(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: ClientCapabilities::new()
                .with_color_support(ColorSupport::TrueColor)
                .with_layers(LayerSet::all()),
        },
    )
    .await;
    let (_, frame) = recv_typed(&mut stream).await;
    let FrameKind::HelloOk { server_caps, .. } = frame else {
        panic!("expected HELLO_OK, got {frame:?}");
    };
    assert!(
        server_caps
            .features
            .contains(ServerFeature::ListDirectoryHost),
        "the server must advertise LIST_DIRECTORY_HOST: {server_caps:?}"
    );
    stream
}

/// Send one `LIST_DIRECTORY` and await its correlated reply.
async fn list_on(
    stream: &mut UnixStream,
    request_id: u32,
    path: &str,
    host: Option<&str>,
) -> DirectoryListingResult {
    send_frame(
        stream,
        &FrameKind::ListDirectory {
            request_id,
            path: path.to_owned(),
            host: host.map(SatelliteHost::new),
        },
    )
    .await;
    loop {
        if let (
            _,
            FrameKind::DirectoryListing {
                request_id: got,
                result,
            },
        ) = recv_typed(stream).await
            && got == request_id
        {
            return result;
        }
    }
}

/// Ask `host` to list `path`, retrying while the hub's link to it is still
/// coming up (the refusal then calls it unreachable), and return the first
/// answer from a connected link.
async fn list_once_linked(
    stream: &mut UnixStream,
    path: &str,
    host: &str,
) -> DirectoryListingResult {
    let deadline = Instant::now() + STEP_DEADLINE;
    let mut request_id = 3000;
    loop {
        let result = list_on(stream, request_id, path, Some(host)).await;
        let linking = matches!(&result, Err(refusal) if refusal.message.contains("is unreachable"));
        if !linking || Instant::now() >= deadline {
            return result;
        }
        request_id += 1;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The routing refusal every failure produces: `OTHER`, never an `ERROR`.
fn refusal(result: DirectoryListingResult) -> DirectoryListingError {
    let refusal = result.expect_err("expected a refusal");
    assert_eq!(refusal.code, DirectoryErrorCode::Other, "{refusal:?}");
    refusal
}

#[test]
fn a_named_satellite_lists_through_the_hub() {
    phux_server_testkit::run_local(async {
        let tmp = TempDir::new().unwrap();
        let tree = tmp.path().join("tree");
        std::fs::create_dir_all(tree.join("beta")).unwrap();
        std::fs::create_dir_all(tree.join("alpha")).unwrap();
        std::fs::write(tree.join("notes.txt"), b"x").unwrap();
        let ws_port = free_port();
        let (sat_shutdown, sat_task) = spawn_satellite(tmp.path().join("sat.sock"), ws_port);
        let (hub_shutdown, hub_task) = spawn_hub(
            tmp.path().join("hub.sock"),
            vec![satellite_entry("sat", ws_port)],
        );
        let mut hub = connect_l3(&tmp.path().join("hub.sock")).await;

        let path = tree.to_str().unwrap();
        let listing = list_once_linked(&mut hub, path, "sat")
            .await
            .expect("the satellite lists its directory through the hub");
        let names: Vec<&str> = listing.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["alpha", "beta"], "directories only, sorted");
        assert_eq!(listing.path, path);
        assert_eq!(listing.parent.as_deref(), tmp.path().to_str());

        // The satellite's own refusal comes back through the relay unchanged,
        // typed, under this request's id.
        let missing = tree.join("missing");
        let refused = list_on(&mut hub, 4000, missing.to_str().unwrap(), Some("sat"))
            .await
            .expect_err("a missing satellite path is refused");
        assert_eq!(refused.code, DirectoryErrorCode::NotFound);

        drop(hub);
        drop(hub_shutdown);
        hub_task.await.unwrap().unwrap();
        drop(sat_shutdown);
        sat_task.await.unwrap().unwrap();
    });
}

#[test]
fn an_unknown_host_or_a_non_hub_server_refuses_naming_the_host() {
    phux_server_testkit::run_local(async {
        let tmp = TempDir::new().unwrap();
        let dead = DeadEndpoint::reserve().await;
        let (hub_shutdown, hub_task) = spawn_hub(
            tmp.path().join("hub.sock"),
            vec![satellite_entry("sat", dead.port())],
        );
        let mut hub = connect_l3(&tmp.path().join("hub.sock")).await;

        let unknown = refusal(list_on(&mut hub, 1, "/", Some("ghost")).await);
        assert_eq!(unknown.path, "/");
        assert!(
            unknown.message.contains("no satellite named ghost"),
            "{}",
            unknown.message
        );

        // A server that is not a hub refuses any host instead of listing its
        // own filesystem under the satellite's name...
        let (plain_shutdown, plain_task) =
            spawn_satellite(tmp.path().join("plain.sock"), free_port());
        let mut plain = connect_l3(&tmp.path().join("plain.sock")).await;
        let not_hub = refusal(list_on(&mut plain, 2, "/", Some("sat")).await);
        assert!(
            not_hub.message.contains("not a federation hub") && not_hub.message.contains("sat"),
            "{}",
            not_hub.message
        );
        // ...while the same request without a host is its own listing.
        list_on(&mut plain, 3, "/", None)
            .await
            .expect("the serving host lists itself");

        drop(plain);
        drop(plain_shutdown);
        plain_task.await.unwrap().unwrap();
        drop(hub);
        drop(hub_shutdown);
        hub_task.await.unwrap().unwrap();
    });
}

#[test]
fn an_unreachable_satellite_is_refused_promptly() {
    phux_server_testkit::run_local(async {
        let tmp = TempDir::new().unwrap();
        // Held dead for the whole test so no neighbour can answer on it.
        let dead = DeadEndpoint::reserve().await;
        let (hub_shutdown, hub_task) = spawn_hub(
            tmp.path().join("hub.sock"),
            vec![satellite_entry("sat", dead.port())],
        );
        let mut hub = connect_l3(&tmp.path().join("hub.sock")).await;

        let started = Instant::now();
        let unreachable = refusal(list_on(&mut hub, 1, "~", Some("sat")).await);
        assert!(
            unreachable.message.contains("sat is unreachable"),
            "{}",
            unreachable.message
        );
        assert_eq!(unreachable.path, "~");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "a down link fails fast, well inside the relay deadline: {:?}",
            started.elapsed()
        );

        drop(hub);
        drop(hub_shutdown);
        hub_task.await.unwrap().unwrap();
    });
}

/// A satellite that completes HELLO advertising `LIST_DIRECTORY`, then never
/// answers a frame: the hub-side shape of a wedged satellite whose link still
/// looks healthy.
async fn silent_satellite() -> (u16, JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let accept = tokio::task::spawn_local(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            tokio::task::spawn_local(swallow_after_hello(tcp));
        }
    });
    (port, accept)
}

async fn swallow_after_hello(tcp: TcpStream) {
    let Ok(mut ws) = tokio_tungstenite::accept_async(tcp).await else {
        return;
    };
    // The hub's HELLO.
    if ws.next().await.is_none() {
        return;
    }
    let hello_ok = FrameKind::HelloOk {
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        server_caps: ServerCapabilities::new()
            .with_layers(LayerSet::all())
            .with_features(ServerFeatureSet::with(&[ServerFeature::ListDirectory])),
        server_id: vec![0; 16],
        selected_profile: BootstrapProfile::SynthesizedVtRaw,
        bootstrap_limits: BootstrapLimits::default(),
    };
    if ws
        .send(Message::Binary(encode_frame(&hello_ok).to_vec().into()))
        .await
        .is_err()
    {
        return;
    }
    // Keep reading so the transport stays up (the WebSocket layer answers
    // pings), and never reply to a frame.
    while let Some(Ok(_)) = ws.next().await {}
}

#[test]
fn a_satellite_that_never_answers_is_refused_at_the_relay_deadline() {
    phux_server_testkit::run_local(async {
        let tmp = TempDir::new().unwrap();
        let (port, silent) = silent_satellite().await;
        let (hub_shutdown, hub_task) = spawn_hub(
            tmp.path().join("hub.sock"),
            vec![satellite_entry("mute", port)],
        );
        let mut hub = connect_l3(&tmp.path().join("hub.sock")).await;

        let started = Instant::now();
        let timed_out = refusal(list_once_linked(&mut hub, "/", "mute").await);
        assert!(
            timed_out
                .message
                .contains("mute did not answer the listing within 10s"),
            "{}",
            timed_out.message
        );
        assert_eq!(timed_out.path, "/");
        assert!(
            started.elapsed() >= Duration::from_secs(10),
            "the refusal waits out the relay deadline, not less"
        );

        drop(hub);
        drop(hub_shutdown);
        hub_task.await.unwrap().unwrap();
        silent.abort();
    });
}
