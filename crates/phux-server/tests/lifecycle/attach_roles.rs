//! ADR-0127 attach roles through the production frame loop: a `VIEWER`
//! subscription is observe-only, widening it is a fresh journaled attach, a
//! `{ PRIMARY, DELIBERATE }` attach seizes the lease in one step with one
//! `SEIZED`, and an attach without the byte behaves exactly as before roles.
//!
//! `owner` attaches the seed pane; `events` is a journal-aware subscription so
//! `ROLE_CHANGED` crosses it (L1 §7.1); `legacy` never sent a cursor, so it is
//! never offered `ROLE_CHANGED`.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::path::Path;
use std::time::Duration;

use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{ClientCapabilities, ColorSupport, LayerSet, ServerFeature};
use phux_protocol::ids::ResourceId;
use phux_protocol::input::InputEvent;
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, CommandValue, ControlAction, ErrorCode, FrameKind,
    InputMode, RolePolicy, SpawnResult, StateScope, TYPE_ATTACHED, TYPE_HELLO_OK,
};
use phux_protocol::wire::info::ResourceInfo;
use tokio::net::UnixStream;
use tokio::time::timeout;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, attach_by_name, await_command_result,
    encode_frame_vec, join_after_shutdown, recv_typed, run_local, send_frame, spawn_server_with,
    wait_for_raw_socket,
};

const SESSION: &str = "demo";
/// How long "nothing else arrives" is watched for. A timing assertion, same
/// discipline as `lease_ttl.rs`.
const QUIET: Duration = Duration::from_millis(300);

/// `HELLO` as `name`; returns whether `HELLO_OK` advertised `ATTACH_ROLES`.
async fn hello(stream: &mut UnixStream, name: &str) -> bool {
    send_frame(
        stream,
        &FrameKind::Hello {
            client_name: name.to_owned(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: ClientCapabilities::new()
                .with_color_support(ColorSupport::TrueColor)
                .with_layers(LayerSet::all()),
        },
    )
    .await;
    let (type_byte, frame) = recv_typed(stream).await;
    assert_eq!(type_byte, TYPE_HELLO_OK);
    let FrameKind::HelloOk { server_caps, .. } = frame else {
        panic!("expected HELLO_OK, got {frame:?}");
    };
    server_caps.features.contains(ServerFeature::AttachRoles)
}

async fn connect(socket: &Path, name: &str) -> UnixStream {
    let mut stream = wait_for_raw_socket(socket, SOCKET_CONNECT_DEADLINE).await;
    assert!(
        hello(&mut stream, name).await,
        "the server advertises ATTACH_ROLES"
    );
    stream
}

/// Session `ATTACH` to `SESSION` declaring `role`; returns every resource the
/// snapshot names.
async fn attach_session(stream: &mut UnixStream, role: Option<RolePolicy>) -> Vec<ResourceId> {
    let FrameKind::Attach {
        attach_id,
        target,
        viewport,
        request_scrollback,
        scrollback_limit_lines,
        ..
    } = attach_by_name(SESSION)
    else {
        unreachable!("attach_by_name builds an ATTACH");
    };
    send_frame(
        stream,
        &FrameKind::Attach {
            attach_id,
            target,
            viewport,
            request_scrollback,
            scrollback_limit_lines,
            role_policy: role,
        },
    )
    .await;
    let (type_byte, frame) = recv_typed(stream).await;
    assert_eq!(type_byte, TYPE_ATTACHED, "got {frame:?}");
    let FrameKind::Attached { snapshot, .. } = frame else {
        panic!("expected ATTACHED, got {frame:?}");
    };
    snapshot.resources.into_iter().map(|info| info.id).collect()
}

/// Attach `SESSION` without a role; returns its focused pane.
async fn attach_pane(stream: &mut UnixStream) -> ResourceId {
    send_frame(stream, &attach_by_name(SESSION)).await;
    let (type_byte, frame) = recv_typed(stream).await;
    assert_eq!(type_byte, TYPE_ATTACHED);
    let FrameKind::Attached { snapshot, .. } = frame else {
        panic!("expected ATTACHED, got {frame:?}");
    };
    snapshot.focused_resource
}

async fn command(stream: &mut UnixStream, request_id: u32, command: Command) -> CommandResult {
    send_frame(
        stream,
        &FrameKind::Command {
            request_id,
            command,
        },
    )
    .await;
    await_command_result(stream, request_id).await
}

fn attach_resource(pane: &ResourceId, role_policy: Option<RolePolicy>) -> Command {
    Command::AttachResource {
        terminal_id: pane.clone(),
        role_policy,
    }
}

fn route_input(pane: &ResourceId) -> Command {
    Command::RouteInput {
        terminal_id: pane.clone(),
        event: InputEvent::Focus(FocusEvent::Gained),
    }
}

fn acquire(pane: &ResourceId) -> Command {
    Command::AcquireInput {
        terminal_id: pane.clone(),
        mode: InputMode::Cooperative,
        ttl_ms: 0,
    }
}

fn is_error(result: &CommandResult, want: ErrorCode) -> bool {
    matches!(result, CommandResult::Error { code, .. } if *code == want)
}

/// `SUBSCRIBE_EVENTS` server-wide, journal-aware when `cursor` is set, then a
/// `GET_STATE` barrier so the subscription is installed before this returns.
async fn subscribe(stream: &mut UnixStream, request_id: u32, cursor: Option<u64>) {
    send_frame(
        stream,
        &FrameKind::SubscribeEvents {
            terminal: None,
            after_seq: cursor,
        },
    )
    .await;
    let result = command(
        stream,
        request_id,
        Command::GetState {
            scope: StateScope::Server,
        },
    )
    .await;
    assert!(
        !matches!(result, CommandResult::Error { .. }),
        "barrier: {result:?}"
    );
}

/// The next `TERMINAL_CONTROL` whose action `matches` accepts.
async fn next_control(
    stream: &mut UnixStream,
    matches: impl Fn(ControlAction) -> bool,
) -> AgentEvent {
    loop {
        let (_, frame) = timeout(WIRE_RECV_TIMEOUT, recv_typed(stream))
            .await
            .expect("the event arrives within the deadline");
        if let FrameKind::Event { event, .. } = frame
            && let AgentEvent::TerminalControl { action, .. } = event
            && matches(action)
        {
            return event;
        }
    }
}

async fn assert_no_control(stream: &mut UnixStream, matches: impl Fn(ControlAction) -> bool) {
    let outcome = timeout(QUIET, next_control(stream, matches)).await;
    assert!(outcome.is_err(), "unexpected TERMINAL_CONTROL: {outcome:?}");
}

fn holder_and_actor(event: &AgentEvent) -> (Option<u32>, Option<u32>) {
    let AgentEvent::TerminalControl {
        input_holder,
        actor,
        ..
    } = event
    else {
        unreachable!("next_control returns TERMINAL_CONTROL");
    };
    (
        input_holder.map(phux_protocol::ClientId::get),
        actor.map(phux_protocol::ClientId::get),
    )
}

async fn pane_state(stream: &mut UnixStream, request_id: u32, pane: &ResourceId) -> ResourceInfo {
    let result = command(
        stream,
        request_id,
        Command::GetState {
            scope: StateScope::Server,
        },
    )
    .await;
    let CommandResult::OkWith(CommandValue::State(snapshot)) = result else {
        panic!("expected GET_STATE OkWith(State), got {result:?}");
    };
    snapshot
        .resources
        .into_iter()
        .find(|r| &r.id == pane)
        .expect("the pane is in the snapshot")
}

/// The next uncorrelated `ERROR`'s code.
async fn next_uncorrelated_error(stream: &mut UnixStream) -> ErrorCode {
    loop {
        let (_, frame) = timeout(WIRE_RECV_TIMEOUT, recv_typed(stream))
            .await
            .expect("the refusal arrives within the deadline");
        if let FrameKind::Error {
            request_id: None,
            code,
            ..
        } = frame
        {
            return code;
        }
    }
}

/// Spawn a second pane into the attached session; returns its id.
async fn spawn_pane(stream: &mut UnixStream, request_id: u32) -> ResourceId {
    send_frame(
        stream,
        &FrameKind::SpawnResource {
            request_id,
            group: phux_server::DEFAULT_GROUP_ID,
            command: Some(vec!["/bin/cat".to_owned()]),
            cwd: None,
            env: None,
            term: None,
            satellite: None,
            owner_terminal: None,
            agent_session: None,
            initial_size: None,
            resource: None,
        },
    )
    .await;
    loop {
        let (_, frame) = timeout(WIRE_RECV_TIMEOUT, recv_typed(stream))
            .await
            .expect("the spawn reply arrives within the deadline");
        if let FrameKind::ResourceSpawned {
            request_id: got,
            result,
            ..
        } = frame
            && got == request_id
        {
            let SpawnResult::Ok(id) = result else {
                panic!("spawn failed: {result:?}");
            };
            return id;
        }
    }
}

fn start() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<Result<(), phux_server::ServerError>>,
) {
    let tmp = tempfile::TempDir::new().unwrap();
    let socket = tmp.path().join("phux.sock");
    let (shutdown, server) = spawn_server_with(socket.clone(), Some(SESSION), |_| {});
    (tmp, socket, shutdown, server)
}

#[test]
fn attach_without_role_policy_is_byte_identical_and_behaves_as_today() {
    // The bytes: an absent role writes nothing, so the body is the tag and
    // the id, the same shape as DETACH_RESOURCE; the frames differ only in
    // the tag.
    let pane = ResourceId::local(7);
    let attach = encode_frame_vec(&FrameKind::Command {
        request_id: 1,
        command: attach_resource(&pane, None),
    });
    let detach = encode_frame_vec(&FrameKind::Command {
        request_id: 1,
        command: Command::DetachResource { terminal_id: pane },
    });
    let differing: Vec<usize> = (0..attach.len().min(detach.len()))
        .filter(|&i| attach[i] != detach[i])
        .collect();
    assert_eq!(attach.len(), detach.len(), "no trailing role byte");
    assert_eq!(differing.len(), 1, "only the command tag differs");
    assert_eq!((attach[differing[0]], detach[differing[0]]), (0x01, 0x02));

    // The behaviour: an ordinary input-capable attach, lease untouched.
    run_local(async {
        let (_tmp, socket, shutdown, server) = start();
        let mut owner = connect(&socket, "owner").await;
        let pane = attach_pane(&mut owner).await;
        let mut client = connect(&socket, "client").await;

        assert_eq!(
            command(&mut client, 1, attach_resource(&pane, None)).await,
            CommandResult::Ok
        );
        assert_eq!(
            command(&mut client, 2, route_input(&pane)).await,
            CommandResult::Ok
        );
        let info = pane_state(&mut client, 3, &pane).await;
        assert_eq!(info.input_holder, None);
        assert!(
            info.viewers.is_empty(),
            "no role, no viewer: {:?}",
            info.viewers
        );

        drop((owner, client));
        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn viewer_input_is_denied_and_acquire_input_is_permission_denied() {
    run_local(async {
        let (_tmp, socket, shutdown, server) = start();
        let mut owner = connect(&socket, "owner").await;
        let pane = attach_pane(&mut owner).await;
        let mut viewer = connect(&socket, "viewer").await;

        let viewing = attach_resource(&pane, Some(RolePolicy::VIEWER));
        assert_eq!(command(&mut viewer, 1, viewing).await, CommandResult::Ok);
        let routed = command(&mut viewer, 2, route_input(&pane)).await;
        assert!(is_error(&routed, ErrorCode::PermissionDenied), "{routed:?}");
        let acquired = command(&mut viewer, 3, acquire(&pane)).await;
        assert!(
            is_error(&acquired, ErrorCode::PermissionDenied),
            "{acquired:?}"
        );
        // Every other input-class command is refused the same way.
        let refused_too = [
            Command::PutFile {
                upload_id: phux_protocol::ids::FileUploadId::new([1; 16]).expect("non-zero"),
                terminal_id: pane.clone(),
                extension: "txt".to_owned(),
                offset: 0,
                data: b"x".to_vec(),
                final_chunk: true,
                sha256: None,
            },
            Command::Transcribe {
                upload_id: phux_protocol::ids::FileUploadId::new([1; 16]).expect("non-zero"),
                terminal_id: pane.clone(),
            },
            Command::ApplyInput {
                operation_id: phux_protocol::ids::InputOperationId::new([2; 16]).expect("non-zero"),
                terminal_id: pane.clone(),
                events: vec![InputEvent::Focus(FocusEvent::Gained)],
            },
        ];
        for (request_id, refused) in (30..).zip(refused_too) {
            let result = command(&mut viewer, request_id, refused).await;
            assert!(is_error(&result, ErrorCode::PermissionDenied), "{result:?}");
        }

        // Fire-and-forget input is dropped with the rate-limited
        // uncorrelated refusal the scope guard uses.
        send_frame(
            &mut viewer,
            &FrameKind::InputFocus {
                terminal_id: pane.clone(),
                event: FocusEvent::Gained,
            },
        )
        .await;
        assert_eq!(
            next_uncorrelated_error(&mut viewer).await,
            ErrorCode::PermissionDenied
        );

        // The role is the viewer's alone, and the inventory lists it.
        assert_eq!(
            command(&mut owner, 4, route_input(&pane)).await,
            CommandResult::Ok
        );
        let info = pane_state(&mut owner, 5, &pane).await;
        assert_eq!(info.viewers.len(), 1, "one viewer: {:?}", info.viewers);
        assert_eq!(info.input_holder, None);

        drop((owner, viewer));
        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn viewer_cannot_widen_without_a_fresh_primary_attach_which_is_journaled() {
    run_local(async {
        let (_tmp, socket, shutdown, server) = start();
        let mut owner = connect(&socket, "owner").await;
        let pane = attach_pane(&mut owner).await;
        let mut events = connect(&socket, "events").await;
        subscribe(&mut events, 1, Some(u64::MAX)).await;
        let mut legacy = connect(&socket, "legacy").await;
        subscribe(&mut legacy, 1, None).await;
        let mut viewer = connect(&socket, "viewer").await;

        let viewing = attach_resource(&pane, Some(RolePolicy::VIEWER));
        assert_eq!(
            command(&mut viewer, 1, viewing.clone()).await,
            CommandResult::Ok
        );
        assert_no_control(&mut events, |action| action == ControlAction::RoleChanged).await;
        let refused = command(&mut viewer, 2, route_input(&pane)).await;
        assert!(
            is_error(&refused, ErrorCode::PermissionDenied),
            "{refused:?}"
        );

        // Widening is a fresh PRIMARY attach, and every watcher sees it.
        let primary = attach_resource(&pane, Some(RolePolicy::PRIMARY));
        assert_eq!(command(&mut viewer, 3, primary).await, CommandResult::Ok);
        let widened =
            next_control(&mut events, |action| action == ControlAction::RoleChanged).await;
        let (holder, actor) = holder_and_actor(&widened);
        assert_eq!(holder, None);
        assert!(actor.is_some(), "the widening names who widened");
        assert_eq!(
            command(&mut viewer, 4, route_input(&pane)).await,
            CommandResult::Ok
        );
        assert!(pane_state(&mut viewer, 5, &pane).await.viewers.is_empty());

        // Narrowing again is a change too.
        assert_eq!(command(&mut viewer, 6, viewing).await, CommandResult::Ok);
        let narrowed =
            next_control(&mut events, |action| action == ControlAction::RoleChanged).await;
        assert_eq!(holder_and_actor(&narrowed).1, actor);

        // A subscription that never proved it decodes this draft is never
        // offered the value it would fail the frame on: the first control it
        // sees is the lease acquired after both flips, not either flip.
        assert_eq!(
            command(&mut owner, 2, acquire(&pane)).await,
            CommandResult::Ok
        );
        let first = next_control(&mut legacy, |action| {
            matches!(action, ControlAction::RoleChanged | ControlAction::Acquired)
        })
        .await;
        assert!(
            matches!(
                first,
                AgentEvent::TerminalControl {
                    action: ControlAction::Acquired,
                    ..
                }
            ),
            "{first:?}"
        );

        drop((owner, events, legacy, viewer));
        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn a_holder_that_narrows_to_viewer_releases_the_lease() {
    run_local(async {
        let (_tmp, socket, shutdown, server) = start();
        let mut owner = connect(&socket, "owner").await;
        let pane = attach_pane(&mut owner).await;
        let mut events = connect(&socket, "events").await;
        subscribe(&mut events, 1, Some(u64::MAX)).await;
        let mut driver = connect(&socket, "driver").await;
        assert_eq!(
            command(&mut driver, 1, attach_resource(&pane, None)).await,
            CommandResult::Ok
        );
        assert_eq!(
            command(&mut driver, 2, acquire(&pane)).await,
            CommandResult::Ok
        );
        let _ = next_control(&mut events, |action| action == ControlAction::Acquired).await;

        let viewing = attach_resource(&pane, Some(RolePolicy::VIEWER));
        assert_eq!(command(&mut driver, 3, viewing).await, CommandResult::Ok);
        let changed =
            next_control(&mut events, |action| action == ControlAction::RoleChanged).await;
        assert_eq!(
            holder_and_actor(&changed).0,
            None,
            "a viewer holds no lease"
        );
        let released = next_control(&mut events, |action| action == ControlAction::Released).await;
        assert_eq!(holder_and_actor(&released).0, None);
        let info = pane_state(&mut owner, 3, &pane).await;
        assert_eq!(info.input_holder, None);
        assert_eq!(info.viewers.len(), 1);
        assert_eq!(
            command(&mut owner, 4, route_input(&pane)).await,
            CommandResult::Ok
        );

        drop((owner, events, driver));
        join_after_shutdown(shutdown, server).await;
    });
}

/// Security review: a `VIEWER` declaration is a per-connection tombstone.
/// Detaching sheds the subscription, not the role, so subscription-free
/// input stays refused; only a fresh `PRIMARY` attach widens, journaled.
#[test]
fn a_detached_viewer_stays_refused_until_a_fresh_primary_attach() {
    run_local(async {
        let (_tmp, socket, shutdown, server) = start();
        let mut owner = connect(&socket, "owner").await;
        let pane = attach_pane(&mut owner).await;
        let mut events = connect(&socket, "events").await;
        subscribe(&mut events, 1, Some(u64::MAX)).await;
        let mut viewer = connect(&socket, "viewer").await;

        let viewing = attach_resource(&pane, Some(RolePolicy::VIEWER));
        assert_eq!(command(&mut viewer, 1, viewing).await, CommandResult::Ok);
        let detach = Command::DetachResource {
            terminal_id: pane.clone(),
        };
        assert_eq!(command(&mut viewer, 2, detach).await, CommandResult::Ok);
        let refused = command(&mut viewer, 3, route_input(&pane)).await;
        assert!(
            is_error(&refused, ErrorCode::PermissionDenied),
            "{refused:?}"
        );

        // A re-attach without the byte declares PRIMARY: a journaled widening.
        assert_eq!(
            command(&mut viewer, 4, attach_resource(&pane, None)).await,
            CommandResult::Ok
        );
        let _ = next_control(&mut events, |action| action == ControlAction::RoleChanged).await;
        assert_eq!(
            command(&mut viewer, 5, route_input(&pane)).await,
            CommandResult::Ok
        );

        drop((owner, events, viewer));
        join_after_shutdown(shutdown, server).await;
    });
}

/// A session `DETACH` ends the attachment, not the declaration.
#[test]
fn a_session_detach_keeps_the_viewer_tombstone() {
    run_local(async {
        let (_tmp, socket, shutdown, server) = start();
        let mut owner = connect(&socket, "owner").await;
        let pane = attach_pane(&mut owner).await;
        let mut viewer = connect(&socket, "viewer").await;
        let _ = attach_session(&mut viewer, Some(RolePolicy::VIEWER)).await;
        send_frame(&mut viewer, &FrameKind::Detach).await;
        let refused = command(&mut viewer, 1, route_input(&pane)).await;
        assert!(
            is_error(&refused, ErrorCode::PermissionDenied),
            "{refused:?}"
        );

        drop((owner, viewer));
        join_after_shutdown(shutdown, server).await;
    });
}

/// A session attached as `VIEWER` watches every pane, its own spawns
/// included; the spawn itself is the grant's to allow.
#[test]
fn a_viewer_sessions_own_spawns_are_observe_only() {
    run_local(async {
        let (_tmp, socket, shutdown, server) = start();
        let mut owner = connect(&socket, "owner").await;
        let _ = attach_pane(&mut owner).await;
        let mut viewer = connect(&socket, "viewer").await;
        let _ = attach_session(&mut viewer, Some(RolePolicy::VIEWER)).await;
        let spawned = spawn_pane(&mut viewer, 7).await;
        let refused = command(&mut viewer, 1, route_input(&spawned)).await;
        assert!(
            is_error(&refused, ErrorCode::PermissionDenied),
            "{refused:?}"
        );
        assert_eq!(
            command(&mut owner, 2, route_input(&spawned)).await,
            CommandResult::Ok
        );

        drop((owner, viewer));
        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn primary_deliberate_attaches_and_seizes_in_one_lock_with_one_seized_broadcast() {
    run_local(async {
        let (_tmp, socket, shutdown, server) = start();
        let mut owner = connect(&socket, "owner").await;
        let pane = attach_pane(&mut owner).await;
        let mut events = connect(&socket, "events").await;
        subscribe(&mut events, 1, Some(u64::MAX)).await;

        assert_eq!(
            command(&mut owner, 1, acquire(&pane)).await,
            CommandResult::Ok
        );
        let acquired = next_control(&mut events, |action| action == ControlAction::Acquired).await;
        let (owner_id, _) = holder_and_actor(&acquired);

        let mut taker = connect(&socket, "taker").await;
        let take = attach_resource(&pane, Some(RolePolicy::TAKEOVER));
        assert_eq!(command(&mut taker, 1, take).await, CommandResult::Ok);
        let seized = next_control(&mut events, |action| action == ControlAction::Seized).await;
        let (holder, actor) = holder_and_actor(&seized);
        assert_eq!(holder, actor, "the attaching client took the wheel");
        assert_ne!(holder, owner_id);
        assert_no_control(&mut events, |_| true).await;

        // The displaced holder stays attached and is simply locked out.
        let locked = command(&mut owner, 2, route_input(&pane)).await;
        assert!(is_error(&locked, ErrorCode::InputLeaseHeld), "{locked:?}");
        assert_eq!(
            command(&mut taker, 2, route_input(&pane)).await,
            CommandResult::Ok
        );
        let info = pane_state(&mut taker, 3, &pane).await;
        assert_eq!(info.input_holder.map(phux_protocol::ClientId::get), holder);

        drop((owner, events, taker));
        join_after_shutdown(shutdown, server).await;
    });
}

/// L9 x ADR-0127: a takeover arms no TTL and supersedes the displaced
/// holder's timer, so the old deadline cannot expire the new holder's lease.
#[test]
fn a_takeover_supersedes_the_prior_holders_ttl() {
    run_local(async {
        let (_tmp, socket, shutdown, server) = start();
        let mut owner = connect(&socket, "owner").await;
        let pane = attach_pane(&mut owner).await;
        let mut events = connect(&socket, "events").await;
        subscribe(&mut events, 1, Some(u64::MAX)).await;
        let timed = Command::AcquireInput {
            terminal_id: pane.clone(),
            mode: InputMode::Cooperative,
            ttl_ms: 200,
        };
        assert_eq!(command(&mut owner, 1, timed).await, CommandResult::Ok);
        let _ = next_control(&mut events, |action| action == ControlAction::Acquired).await;

        let mut taker = connect(&socket, "taker").await;
        let take = attach_resource(&pane, Some(RolePolicy::TAKEOVER));
        assert_eq!(command(&mut taker, 1, take).await, CommandResult::Ok);
        let seized = next_control(&mut events, |action| action == ControlAction::Seized).await;
        let (holder, _) = holder_and_actor(&seized);
        let expired = timeout(
            Duration::from_millis(600),
            next_control(&mut events, |action| action == ControlAction::Expired),
        )
        .await;
        assert!(
            expired.is_err(),
            "the displaced holder's TTL fired: {expired:?}"
        );
        let info = pane_state(&mut taker, 2, &pane).await;
        assert_eq!(info.input_holder.map(phux_protocol::ClientId::get), holder);

        drop((owner, events, taker));
        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn primary_never_leaves_the_lease_untouched() {
    run_local(async {
        let (_tmp, socket, shutdown, server) = start();
        let mut owner = connect(&socket, "owner").await;
        let pane = attach_pane(&mut owner).await;
        let mut events = connect(&socket, "events").await;
        subscribe(&mut events, 1, Some(u64::MAX)).await;
        assert_eq!(
            command(&mut owner, 1, acquire(&pane)).await,
            CommandResult::Ok
        );
        let acquired = next_control(&mut events, |action| action == ControlAction::Acquired).await;
        let (owner_id, _) = holder_and_actor(&acquired);

        let mut other = connect(&socket, "other").await;
        let primary = attach_resource(&pane, Some(RolePolicy::PRIMARY));
        assert_eq!(command(&mut other, 1, primary).await, CommandResult::Ok);
        assert_no_control(&mut events, |_| true).await;
        let held = command(&mut other, 2, route_input(&pane)).await;
        assert!(is_error(&held, ErrorCode::InputLeaseHeld), "{held:?}");
        let info = pane_state(&mut other, 3, &pane).await;
        assert_eq!(
            info.input_holder.map(phux_protocol::ClientId::get),
            owner_id
        );

        drop((owner, events, other));
        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn session_attach_role_applies_to_every_returned_terminal() {
    run_local(async {
        let (_tmp, socket, shutdown, server) = start();
        let mut owner = connect(&socket, "owner").await;
        let first = attach_pane(&mut owner).await;
        let second = spawn_pane(&mut owner, 7).await;

        let mut viewer = connect(&socket, "viewer").await;
        let returned = attach_session(&mut viewer, Some(RolePolicy::VIEWER)).await;
        assert!(
            returned.contains(&first) && returned.contains(&second),
            "{returned:?}"
        );
        for (request_id, pane) in [(1, &first), (2, &second)] {
            let refused = command(&mut viewer, request_id, route_input(pane)).await;
            assert!(
                is_error(&refused, ErrorCode::PermissionDenied),
                "{pane:?}: {refused:?}"
            );
        }

        let mut taker = connect(&socket, "taker").await;
        let _ = attach_session(&mut taker, Some(RolePolicy::TAKEOVER)).await;
        let first_holder = pane_state(&mut taker, 1, &first).await.input_holder;
        let second_holder = pane_state(&mut taker, 2, &second).await.input_holder;
        assert!(
            first_holder.is_some(),
            "the takeover seized every returned pane"
        );
        assert_eq!(first_holder, second_holder);
        assert_eq!(
            command(&mut taker, 3, route_input(&second)).await,
            CommandResult::Ok
        );

        drop((owner, viewer, taker));
        join_after_shutdown(shutdown, server).await;
    });
}

#[test]
fn a_deliberate_viewer_is_refused_on_both_attaches() {
    run_local(async {
        let (_tmp, socket, shutdown, server) = start();
        let mut owner = connect(&socket, "owner").await;
        let pane = attach_pane(&mut owner).await;
        let invalid = RolePolicy::from_u8(RolePolicy::VIEWER_BIT | RolePolicy::DELIBERATE_BIT);

        let mut client = connect(&socket, "client").await;
        let refused = command(&mut client, 1, attach_resource(&pane, Some(invalid))).await;
        assert!(is_error(&refused, ErrorCode::InvalidCommand), "{refused:?}");
        assert!(pane_state(&mut client, 2, &pane).await.viewers.is_empty());

        let FrameKind::Attach {
            attach_id,
            target,
            viewport,
            request_scrollback,
            scrollback_limit_lines,
            ..
        } = attach_by_name(SESSION)
        else {
            unreachable!("attach_by_name builds an ATTACH");
        };
        send_frame(
            &mut client,
            &FrameKind::Attach {
                attach_id,
                target,
                viewport,
                request_scrollback,
                scrollback_limit_lines,
                role_policy: Some(invalid),
            },
        )
        .await;
        assert_eq!(
            next_uncorrelated_error(&mut client).await,
            ErrorCode::MalformedMessage
        );

        drop((owner, client));
        join_after_shutdown(shutdown, server).await;
    });
}
