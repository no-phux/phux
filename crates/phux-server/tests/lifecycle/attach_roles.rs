//! ADR-0127 attach roles through the production frame loop: a `VIEWER` is
//! observe-only per connection (a tombstone detach does not shed), widening
//! is a fresh journaled `PRIMARY` attach, `{ PRIMARY, DELIBERATE }` seizes
//! in one step with one `SEIZED`, and no role byte behaves as before roles.
//! `events` subscriptions are journal-aware so `ROLE_CHANGED` crosses them
//! (L1 §7.1); a `legacy` one never sent a cursor and is never offered it.

use std::time::Duration;

use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{ClientCapabilities, ServerFeature};
use phux_protocol::ids::{FileUploadId, InputOperationId, ResourceId};
use phux_protocol::input::InputEvent;
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, ControlAction, ErrorCode, FrameKind, InputMode, RolePolicy,
};
use phux_protocol::wire::info::ResourceInfo;
use phux_server_testkit::{attach_by_name, command, encode_frame_vec, recv_typed, send_frame};
use tokio::net::UnixStream;
use tokio::time::timeout;

use crate::common::{
    Seen, Server, attach, attach_frame, find, next_event, sh, spawned, state, subscribe, wait_frame,
};

const SESSION: &str = "demo";
/// How long "nothing else arrives" is watched for (a timing assertion).
const QUIET: Duration = Duration::from_millis(300);

/// Session `ATTACH` to `SESSION` declaring `role`; returns its resources.
async fn attach_session(stream: &mut UnixStream, role: Option<RolePolicy>) -> Vec<ResourceId> {
    let mut frame = attach_by_name(SESSION);
    if let FrameKind::Attach { role_policy, .. } = &mut frame {
        *role_policy = role;
    }
    attach_frame(stream, &frame)
        .await
        .resources
        .into_iter()
        .map(|info| info.id)
        .collect()
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

fn acquire(pane: &ResourceId, ttl_ms: u32) -> Command {
    Command::AcquireInput {
        terminal_id: pane.clone(),
        mode: InputMode::Cooperative,
        ttl_ms,
    }
}

async fn assert_code(stream: &mut UnixStream, request_id: u32, cmd: Command, want: ErrorCode) {
    let result = command(stream, request_id, cmd).await;
    assert!(
        matches!(&result, CommandResult::Error { code, .. } if *code == want),
        "expected {want:?}, got {result:?}"
    );
}

async fn assert_ok(stream: &mut UnixStream, request_id: u32, cmd: Command) {
    assert_eq!(command(stream, request_id, cmd).await, CommandResult::Ok);
}

/// The next `TERMINAL_CONTROL` with `action`: (holder, actor) client ids.
async fn control(stream: &mut UnixStream, action: ControlAction) -> (Option<u32>, Option<u32>) {
    let event = next_event(
        stream,
        |e: &Seen| matches!(e.event, AgentEvent::TerminalControl { action: a, .. } if a == action),
    )
    .await;
    let AgentEvent::TerminalControl {
        input_holder,
        actor,
        ..
    } = event.event
    else {
        unreachable!()
    };
    (
        input_holder.map(phux_protocol::ClientId::get),
        actor.map(phux_protocol::ClientId::get),
    )
}

/// No `TERMINAL_CONTROL` accepted by `pred` arrives within `quiet`.
async fn assert_no_control(
    stream: &mut UnixStream,
    quiet: Duration,
    pred: impl Fn(ControlAction) -> bool,
) {
    let found = timeout(quiet, async {
        loop {
            if let (
                _,
                FrameKind::Event {
                    event: AgentEvent::TerminalControl { action, .. },
                    ..
                },
            ) = recv_typed(stream).await
                && pred(action)
            {
                return action;
            }
        }
    })
    .await;
    assert!(found.is_err(), "unexpected TERMINAL_CONTROL: {found:?}");
}

async fn pane_state(stream: &mut UnixStream, request_id: u32, pane: &ResourceId) -> ResourceInfo {
    find(&state(stream, request_id).await, pane)
        .cloned()
        .expect("the pane is listed")
}

async fn next_uncorrelated_error(stream: &mut UnixStream) -> ErrorCode {
    wait_frame(stream, "uncorrelated ERROR", |frame| match frame {
        FrameKind::Error {
            request_id: None,
            code,
            ..
        } => Some(code),
        _ => None,
    })
    .await
}

/// A server with `owner` attached to the seed pane and a journal-aware
/// `events` watcher.
async fn start() -> (Server, UnixStream, ResourceId, UnixStream) {
    let server = Server::start(Some(SESSION), |_| {});
    let mut owner = server.connect().await;
    let pane = attach(&mut owner, SESSION).await.focused_resource;
    let mut events = server.connect().await;
    subscribe(&mut events, 1, None, Some(u64::MAX)).await;
    (server, owner, pane, events)
}

#[test]
fn attach_without_role_policy_is_byte_identical_and_behaves_as_before() {
    // No role writes nothing: ATTACH_RESOURCE and DETACH_RESOURCE differ
    // only in the command tag.
    let pane = ResourceId::local(7);
    let encode = |command| {
        encode_frame_vec(&FrameKind::Command {
            request_id: 1,
            command,
        })
    };
    let attach_bytes = encode(attach_resource(&pane, None));
    let detach_bytes = encode(Command::DetachResource { terminal_id: pane });
    assert_eq!(
        attach_bytes.len(),
        detach_bytes.len(),
        "no trailing role byte"
    );
    let differing: Vec<usize> = (0..attach_bytes.len())
        .filter(|&i| attach_bytes[i] != detach_bytes[i])
        .collect();
    assert_eq!(differing.len(), 1);
    assert_eq!(
        (attach_bytes[differing[0]], detach_bytes[differing[0]]),
        (0x01, 0x02)
    );

    phux_server_testkit::run_local(async {
        let server = Server::start(Some(SESSION), |_| {});
        // The server advertises the feature in HELLO_OK.
        let mut client = phux_server_testkit::wait_for_raw_socket(
            &server.socket,
            phux_server_testkit::SOCKET_CONNECT_DEADLINE,
        )
        .await;
        let hello = FrameKind::Hello {
            client_name: "roles".to_owned(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: ClientCapabilities::new(),
        };
        send_frame(&mut client, &hello).await;
        let FrameKind::HelloOk { server_caps, .. } = recv_typed(&mut client).await.1 else {
            panic!("expected HELLO_OK");
        };
        assert!(server_caps.features.contains(ServerFeature::AttachRoles));

        let mut owner = server.connect().await;
        let pane = attach(&mut owner, SESSION).await.focused_resource;
        assert_ok(&mut client, 1, attach_resource(&pane, None)).await;
        assert_ok(&mut client, 2, route_input(&pane)).await;
        let info = pane_state(&mut client, 3, &pane).await;
        assert_eq!(info.input_holder, None);
        assert!(info.viewers.is_empty(), "no role, no viewer");

        drop((owner, client));
        server.stop().await;
    });
}

#[test]
fn viewer_input_of_every_kind_is_permission_denied() {
    phux_server_testkit::run_local(async {
        let (server, mut owner, pane, events) = start().await;
        let mut viewer = server.connect().await;
        assert_ok(
            &mut viewer,
            1,
            attach_resource(&pane, Some(RolePolicy::VIEWER)),
        )
        .await;
        let upload_id = FileUploadId::new([1; 16]).unwrap();
        let refused = [
            route_input(&pane),
            acquire(&pane, 0),
            Command::PutFile {
                upload_id,
                terminal_id: pane.clone(),
                extension: "txt".to_owned(),
                offset: 0,
                data: b"x".to_vec(),
                final_chunk: true,
                sha256: None,
            },
            Command::Transcribe {
                upload_id,
                terminal_id: pane.clone(),
            },
            Command::ApplyInput {
                operation_id: InputOperationId::new([2; 16]).unwrap(),
                terminal_id: pane.clone(),
                events: vec![InputEvent::Focus(FocusEvent::Gained)],
            },
        ];
        for (request_id, cmd) in (30..).zip(refused) {
            assert_code(&mut viewer, request_id, cmd, ErrorCode::PermissionDenied).await;
        }
        // Fire-and-forget input gets the uncorrelated refusal.
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
        assert_ok(&mut owner, 4, route_input(&pane)).await;
        let info = pane_state(&mut owner, 5, &pane).await;
        assert_eq!(info.viewers.len(), 1);
        assert_eq!(info.input_holder, None);

        drop((owner, events, viewer));
        server.stop().await;
    });
}

/// Widening needs a fresh `PRIMARY` attach, which every journal-aware
/// watcher sees (as does narrowing); a legacy subscription is never offered
/// `ROLE_CHANGED`. A resource or session detach sheds the subscription, not
/// the role: a detached viewer stays refused until it re-attaches.
#[test]
fn widening_is_a_journaled_fresh_attach_and_detach_keeps_the_tombstone() {
    phux_server_testkit::run_local(async {
        let (server, mut owner, pane, mut events) = start().await;
        let mut legacy = server.connect().await;
        subscribe(&mut legacy, 1, None, None).await;
        let mut viewer = server.connect().await;
        let viewing = attach_resource(&pane, Some(RolePolicy::VIEWER));

        assert_ok(&mut viewer, 1, viewing.clone()).await;
        assert_no_control(&mut events, QUIET, |a| a == ControlAction::RoleChanged).await;
        assert_code(
            &mut viewer,
            2,
            route_input(&pane),
            ErrorCode::PermissionDenied,
        )
        .await;
        assert_ok(
            &mut viewer,
            3,
            attach_resource(&pane, Some(RolePolicy::PRIMARY)),
        )
        .await;
        let (holder, actor) = control(&mut events, ControlAction::RoleChanged).await;
        assert_eq!(holder, None);
        assert!(actor.is_some(), "the widening names who widened");
        assert_ok(&mut viewer, 4, route_input(&pane)).await;
        assert!(pane_state(&mut viewer, 5, &pane).await.viewers.is_empty());
        assert_ok(&mut viewer, 6, viewing.clone()).await;
        assert_eq!(
            control(&mut events, ControlAction::RoleChanged).await.1,
            actor,
            "narrowing too"
        );

        // The legacy subscription's first control is the later acquire.
        assert_ok(&mut owner, 2, acquire(&pane, 0)).await;
        let first = next_event(&mut legacy, |e: &Seen| {
            matches!(e.event, AgentEvent::TerminalControl { .. })
        })
        .await;
        assert!(
            matches!(
                first.event,
                AgentEvent::TerminalControl {
                    action: ControlAction::Acquired,
                    ..
                }
            ),
            "{first:?}"
        );
        assert_ok(
            &mut owner,
            3,
            Command::ReleaseInput {
                terminal_id: pane.clone(),
            },
        )
        .await;

        // Detached, the viewer's declaration stands.
        assert_ok(
            &mut viewer,
            7,
            Command::DetachResource {
                terminal_id: pane.clone(),
            },
        )
        .await;
        assert_code(
            &mut viewer,
            8,
            route_input(&pane),
            ErrorCode::PermissionDenied,
        )
        .await;
        assert_ok(&mut viewer, 9, attach_resource(&pane, None)).await;
        control(&mut events, ControlAction::RoleChanged).await;
        assert_ok(&mut viewer, 10, route_input(&pane)).await;

        let mut session_viewer = server.connect().await;
        attach_session(&mut session_viewer, Some(RolePolicy::VIEWER)).await;
        send_frame(&mut session_viewer, &FrameKind::Detach).await;
        assert_code(
            &mut session_viewer,
            1,
            route_input(&pane),
            ErrorCode::PermissionDenied,
        )
        .await;

        drop((owner, events, legacy, viewer, session_viewer));
        server.stop().await;
    });
}

#[test]
fn a_holder_that_narrows_to_viewer_releases_the_lease() {
    phux_server_testkit::run_local(async {
        let (server, mut owner, pane, mut events) = start().await;
        let mut driver = server.connect().await;
        assert_ok(&mut driver, 1, attach_resource(&pane, None)).await;
        assert_ok(&mut driver, 2, acquire(&pane, 0)).await;
        control(&mut events, ControlAction::Acquired).await;

        assert_ok(
            &mut driver,
            3,
            attach_resource(&pane, Some(RolePolicy::VIEWER)),
        )
        .await;
        assert_eq!(
            control(&mut events, ControlAction::RoleChanged).await.0,
            None,
            "a viewer holds no lease"
        );
        assert_eq!(control(&mut events, ControlAction::Released).await.0, None);
        let info = pane_state(&mut owner, 3, &pane).await;
        assert_eq!((info.input_holder, info.viewers.len()), (None, 1));
        assert_ok(&mut owner, 4, route_input(&pane)).await;

        drop((owner, events, driver));
        server.stop().await;
    });
}

/// `TAKEOVER` seizes in one step with exactly one `SEIZED`, arms no TTL, and
/// supersedes the displaced holder's timer; the displaced holder stays
/// attached but locked out. Plain `PRIMARY` never touches the lease.
#[test]
fn takeover_seizes_once_superseding_the_prior_ttl_and_primary_does_not() {
    phux_server_testkit::run_local(async {
        let (server, mut owner, pane, mut events) = start().await;
        assert_ok(&mut owner, 1, acquire(&pane, 0)).await;
        let (owner_id, _) = control(&mut events, ControlAction::Acquired).await;

        let mut other = server.connect().await;
        assert_ok(
            &mut other,
            1,
            attach_resource(&pane, Some(RolePolicy::PRIMARY)),
        )
        .await;
        assert_no_control(&mut events, QUIET, |_| true).await;
        assert_code(&mut other, 2, route_input(&pane), ErrorCode::InputLeaseHeld).await;
        assert_eq!(
            pane_state(&mut other, 3, &pane)
                .await
                .input_holder
                .map(phux_protocol::ClientId::get),
            owner_id
        );

        // Re-arm the holder's lease with a TTL the takeover must supersede.
        assert_ok(&mut owner, 3, acquire(&pane, 200)).await;
        control(&mut events, ControlAction::Acquired).await;

        let mut taker = server.connect().await;
        assert_ok(
            &mut taker,
            1,
            attach_resource(&pane, Some(RolePolicy::TAKEOVER)),
        )
        .await;
        let (holder, actor) = control(&mut events, ControlAction::Seized).await;
        assert_eq!(holder, actor, "the attaching client took the wheel");
        assert_ne!(holder, owner_id);
        // Nothing else: no second SEIZED, and the old 200ms TTL never expires it.
        assert_no_control(&mut events, Duration::from_millis(600), |_| true).await;
        assert_code(&mut owner, 4, route_input(&pane), ErrorCode::InputLeaseHeld).await;
        assert_ok(&mut taker, 2, route_input(&pane)).await;
        assert_eq!(
            pane_state(&mut taker, 3, &pane)
                .await
                .input_holder
                .map(phux_protocol::ClientId::get),
            holder
        );

        drop((owner, events, other, taker));
        server.stop().await;
    });
}

/// A session attach's role covers every returned Terminal, including the
/// viewer session's own later spawns; a session `TAKEOVER` seizes them all.
#[test]
fn a_session_role_applies_to_every_terminal_it_returns_or_spawns() {
    phux_server_testkit::run_local(async {
        let (server, mut owner, first, events) = start().await;
        let second = spawned(&mut owner, 7, sh("cat")).await;

        let mut viewer = server.connect().await;
        let returned = attach_session(&mut viewer, Some(RolePolicy::VIEWER)).await;
        assert!(
            returned.contains(&first) && returned.contains(&second),
            "{returned:?}"
        );
        let own = spawned(&mut viewer, 7, sh("cat")).await;
        for (request_id, pane) in [(1, &first), (2, &second), (3, &own)] {
            assert_code(
                &mut viewer,
                request_id,
                route_input(pane),
                ErrorCode::PermissionDenied,
            )
            .await;
        }
        assert_ok(&mut owner, 2, route_input(&own)).await;

        let mut taker = server.connect().await;
        attach_session(&mut taker, Some(RolePolicy::TAKEOVER)).await;
        let first_holder = pane_state(&mut taker, 1, &first).await.input_holder;
        assert!(
            first_holder.is_some(),
            "the takeover seized every returned pane"
        );
        assert_eq!(
            first_holder,
            pane_state(&mut taker, 2, &second).await.input_holder
        );

        drop((owner, events, viewer, taker));
        server.stop().await;
    });
}

#[test]
fn a_deliberate_viewer_is_refused_on_both_attaches() {
    phux_server_testkit::run_local(async {
        let (server, owner, pane, events) = start().await;
        let invalid = RolePolicy::from_u8(RolePolicy::VIEWER_BIT | RolePolicy::DELIBERATE_BIT);
        let mut client = server.connect().await;
        assert_code(
            &mut client,
            1,
            attach_resource(&pane, Some(invalid)),
            ErrorCode::InvalidCommand,
        )
        .await;
        assert!(pane_state(&mut client, 2, &pane).await.viewers.is_empty());

        let mut frame = attach_by_name(SESSION);
        if let FrameKind::Attach { role_policy, .. } = &mut frame {
            *role_policy = Some(invalid);
        }
        send_frame(&mut client, &frame).await;
        assert_eq!(
            next_uncorrelated_error(&mut client).await,
            ErrorCode::MalformedMessage
        );

        drop((owner, events, client));
        server.stop().await;
    });
}
