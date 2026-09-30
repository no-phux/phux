//! `LIST_DIRECTORY.host` through the hub (`docs/spec/L3.md` §4.1): a relayed
//! listing, an unknown host, a non-hub server, an unreachable satellite, and
//! a satellite that completes its handshake and then never answers.

use super::*;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, LayerSet, ServerCapabilities, ServerFeature,
    ServerFeatureSet,
};
use phux_protocol::wire::frame::{
    DirectoryErrorCode, DirectoryListingError, DirectoryListingResult,
};

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
    phux_server_testkit::recv_until(stream, |_, frame| match frame {
        FrameKind::DirectoryListing {
            request_id: got,
            result,
        } if got == request_id => Some(result),
        _ => None,
    })
    .await
}

/// List on `host`, retrying while the link is still coming up.
async fn list_once_linked(
    stream: &mut UnixStream,
    path: &str,
    host: &str,
) -> DirectoryListingResult {
    let deadline = Instant::now() + STEP_DEADLINE;
    for request_id in 3000.. {
        let result = list_on(stream, request_id, path, Some(host)).await;
        let linking = matches!(&result, Err(refusal) if refusal.message.contains("is unreachable"));
        if !linking || Instant::now() >= deadline {
            return result;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    unreachable!()
}

/// Every routing failure is an `OTHER` refusal, never an `ERROR`.
fn refusal(result: DirectoryListingResult) -> DirectoryListingError {
    let refusal = result.expect_err("expected a refusal");
    assert_eq!(refusal.code, DirectoryErrorCode::Other, "{refusal:?}");
    refusal
}

#[test]
fn a_named_satellite_lists_through_the_hub() {
    phux_server_testkit::run_local(async {
        let fed = Fed::boot().await;
        let tree = fed.tmp.path().join("tree");
        std::fs::create_dir_all(tree.join("beta")).unwrap();
        std::fs::create_dir_all(tree.join("alpha")).unwrap();
        std::fs::write(tree.join("notes.txt"), b"x").unwrap();
        let mut hub = fed.hub().await;

        let path = tree.to_str().unwrap();
        let listing = list_once_linked(&mut hub, path, "sat")
            .await
            .expect("the satellite lists its directory through the hub");
        let names: Vec<&str> = listing.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["alpha", "beta"], "directories only, sorted");
        assert_eq!(listing.path, path);
        assert_eq!(listing.parent.as_deref(), fed.tmp.path().to_str());

        // The satellite's own typed refusal relays unchanged.
        let missing = tree.join("missing");
        let refused = list_on(&mut hub, 4000, missing.to_str().unwrap(), Some("sat"))
            .await
            .expect_err("a missing satellite path is refused");
        assert_eq!(refused.code, DirectoryErrorCode::NotFound);

        drop(hub);
        fed.shutdown().await;
    });
}

#[test]
fn unknown_unreachable_and_non_hub_hosts_are_refused_naming_the_host() {
    phux_server_testkit::run_local(async {
        let tmp = TempDir::new().unwrap();
        let dead = DeadEndpoint::reserve().await;
        let (hub_shutdown, hub_task) = spawn_hub(
            tmp.path().join("hub.sock"),
            vec![satellite_entry("sat", dead.port())],
        );
        let mut hub = wait_for_socket(&tmp.path().join("hub.sock"), STEP_DEADLINE).await;

        let unknown = refusal(list_on(&mut hub, 1, "/", Some("ghost")).await);
        assert_eq!(unknown.path, "/");
        assert!(
            unknown.message.contains("no satellite named ghost"),
            "{}",
            unknown.message
        );

        let started = Instant::now();
        let unreachable = refusal(list_on(&mut hub, 2, "~", Some("sat")).await);
        assert!(
            unreachable.message.contains("sat is unreachable"),
            "{}",
            unreachable.message
        );
        assert_eq!(unreachable.path, "~");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "a down link fails fast"
        );

        // A non-hub server refuses any host rather than listing itself under it.
        let (_port, plain_shutdown, plain_task) =
            spawn_satellite(tmp.path().join("plain.sock")).await;
        let mut plain = wait_for_socket(&tmp.path().join("plain.sock"), STEP_DEADLINE).await;
        let not_hub = refusal(list_on(&mut plain, 3, "/", Some("sat")).await);
        assert!(
            not_hub.message.contains("not a federation hub") && not_hub.message.contains("sat"),
            "{}",
            not_hub.message
        );
        list_on(&mut plain, 4, "/", None)
            .await
            .expect("the serving host lists itself");

        drop((plain, hub));
        stop(plain_shutdown, plain_task).await;
        stop(hub_shutdown, hub_task).await;
    });
}

/// A satellite that completes HELLO advertising `LIST_DIRECTORY`, then never
/// answers a frame: a wedged satellite whose link still looks healthy.
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
    // Keep reading (the WebSocket layer answers pings) and never reply.
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
        let mut hub = wait_for_socket(&tmp.path().join("hub.sock"), STEP_DEADLINE).await;

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
            "waits out the relay deadline"
        );

        drop(hub);
        stop(hub_shutdown, hub_task).await;
        silent.abort();
    });
}
