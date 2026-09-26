//! Real embedded engine/control tests of the ticket boundary, not GPU receipts.

use super::*;
use phux_client_runtime::{Runtime, control::ControlOptions, engine::Scroll};
use phux_protocol::{
    PROTOCOL_VERSION, ResourceId,
    caps::{
        BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, ServerCapabilities,
        ServerFeature, ServerFeatureSet,
    },
    ids::{BootstrapId, ClientId, SessionId, StreamId, WindowId},
    wire::{
        frame::{AttachTarget, Command, CommandResult, ErrorCode, FrameKind},
        info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo},
    },
};

const HANDLE: &str = "phux-client:fixture";

fn terminal() -> ResourceId {
    ResourceId::local(7)
}

fn ready() -> (Client, ViewId, Surface) {
    let client = Runtime::embedded(ControlOptions {
        attach: Some(AttachTarget::ByName("presentation".into())),
        ..ControlOptions::default()
    });
    bootstrap(&client, BootstrapLimits::default());
    let view = client.create_view(&terminal()).expect("view");
    let mut surface = Surface::default();
    surface.bind_window(17);
    (client, view, surface)
}

fn bootstrap(client: &Client, limits: BootstrapLimits) {
    client.with_control(ControlPlane::connection_opened);
    let _ = client.take_outbound();
    client
        .feed(FrameKind::HelloOk {
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            server_caps: ServerCapabilities::new()
                .with_features(ServerFeatureSet::with(&[ServerFeature::AcknowledgedInput])),
            server_id: vec![1; 16],
            selected_profile: BootstrapProfile::SynthesizedVtRaw,
            bootstrap_limits: limits,
        })
        .expect("hello");
    let attach_id = client
        .take_outbound()
        .iter()
        .find_map(|bytes| match FrameKind::decode(bytes).expect("decode").0 {
            FrameKind::Attach { attach_id, .. } => Some(attach_id),
            _ => None,
        })
        .expect("attach request");
    finish_bootstrap(client, attach_id);
    assert!(client.with_control(|control| control.engine().unwrap().input_ready(&terminal())));
    let _ = client.take_outbound();
    let _ = client.take_events();
}

fn finish_bootstrap(client: &Client, attach_id: u32) {
    let stream_id = StreamId::new(1).unwrap();
    let bootstrap_id = BootstrapId::new(1).unwrap();
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), terminal())
        .with_sessions(vec![SessionInfo::new(SessionId::new(1), "presentation")])
        .with_windows(vec![WindowInfo::new(
            WindowId::new(1),
            SessionId::new(1),
            "shell",
        )])
        .with_resources(vec![ResourceInfo::new(terminal(), WindowId::new(1), 20, 4)]);
    for frame in [
        FrameKind::Attached {
            attach_id,
            snapshot,
            initial_client_id: ClientId::new(1),
        },
        FrameKind::BootstrapBegin {
            terminal_id: terminal(),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 20,
            rows: 4,
            base_seq: 0,
        },
        FrameKind::BootstrapChunk {
            terminal_id: terminal(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: b"history\r\n1\r\n2\r\n3\r\n4\r\nready".to_vec().into(),
        },
        FrameKind::BootstrapReady {
            terminal_id: terminal(),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
        FrameKind::AttachReady { attach_id },
    ] {
        client.feed(frame).expect("bootstrap");
    }
}

fn unknown(client: &Client) -> ProjectionFence {
    client.with_control(|control| {
        control.apply_line(&terminal(), "command");
        let request_id = control
            .take_outbound()
            .iter()
            .find_map(|bytes| match FrameKind::decode(bytes).expect("decode").0 {
                FrameKind::Command {
                    request_id,
                    command: Command::ApplyInput { .. },
                } => Some(request_id),
                _ => None,
            })
            .expect("apply input");
        control
            .feed(FrameKind::CommandResult {
                request_id,
                result: CommandResult::Error {
                    code: ErrorCode::InputDeliveryUnknown,
                    message: "ambiguous fixture delivery".into(),
                },
            })
            .expect("unknown");
        control.projection_fence(&terminal()).expect("fence")
    })
}

fn capture(client: &Client, view: ViewId, surface: &Surface) -> Result<Ticket, Rejection> {
    client.with_control(|control| {
        Ticket::capture(
            HANDLE,
            &phux_client_ffi::projection::id::encode(&terminal()),
            view,
            17,
            &surface.lifetime,
            control,
        )
    })
}

fn clear(client: &Client, ticket: &Ticket) -> Result<(), Rejection> {
    client.with_control(|control| ticket.acknowledge_current(control))
}

fn fenced(client: &Client) -> bool {
    client.with_control(|control| control.delivery_fenced(&terminal()))
}

#[test]
fn acquisition_and_cancelled_paint_leave_the_fence() {
    let (client, view, surface) = ready();
    let fence = unknown(&client);
    assert!(
        !client.input_ready(&terminal()),
        "delivery fence blocks input, not recovery"
    );
    let ticket = capture(&client, view, &surface).unwrap();
    assert_eq!(ticket.fence, Some(fence));
    assert!(Arc::ptr_eq(
        &ticket.frame(),
        &client.acquire_view(view).unwrap()
    ));
    drop(ticket);
    assert!(fenced(&client));
}

#[test]
fn current_ticket_can_conditionally_clear_only_once() {
    let (client, view, surface) = ready();
    unknown(&client);
    let ticket = capture(&client, view, &surface).unwrap();
    assert_eq!(clear(&client, &ticket), Ok(()));
    assert!(!fenced(&client));
    assert_eq!(clear(&client, &ticket), Err(Rejection::StaleFence));
}

#[test]
fn newer_same_connection_delivery_cannot_be_cleared_by_old_ticket() {
    let (client, view, surface) = ready();
    let first = unknown(&client);
    let ticket = capture(&client, view, &surface).unwrap();
    assert!(client.with_control(|control| control.acknowledge_projection_if(&terminal(), first)));
    let second = unknown(&client);
    assert_eq!(first.connection_epoch, second.connection_epoch);
    assert_ne!(first.delivery_id, second.delivery_id);
    assert_eq!(clear(&client, &ticket), Err(Rejection::StaleFence));
    assert!(fenced(&client));
}

#[test]
fn reconnect_rejects_late_ticket_and_retained_view() {
    let (client, view, surface) = ready();
    unknown(&client);
    let ticket = capture(&client, view, &surface).unwrap();
    client.with_control(|control| control.connection_lost(None));
    assert_eq!(clear(&client, &ticket), Err(Rejection::Detached));
    assert!(capture(&client, view, &surface).is_err());
    bootstrap(&client, BootstrapLimits::new(1024, 1024).unwrap());
    assert_eq!(clear(&client, &ticket), Err(Rejection::StaleFence));
    assert!(
        capture(&client, view, &surface).is_err(),
        "old view is not a current engine member"
    );
    assert!(fenced(&client));
}

#[test]
fn removed_view_cannot_acknowledge_its_retained_frame() {
    let (client, view, surface) = ready();
    unknown(&client);
    let ticket = capture(&client, view, &surface).unwrap();
    client.destroy_view(view).unwrap();
    assert_eq!(clear(&client, &ticket), Err(Rejection::StaleView));
    assert!(!ticket.frame.text().is_empty());
    assert!(fenced(&client));
}

#[test]
fn target_change_invalidates_pending_root_without_waiting_for_render() {
    let (client, view, mut surface) = ready();
    unknown(&client);
    let ticket = capture(&client, view, &surface).unwrap();
    surface.invalidate();
    assert_eq!(clear(&client, &ticket), Err(Rejection::StaleSurface));
    assert!(fenced(&client));
}

#[test]
fn destroyed_surface_does_not_survive_in_a_ticket() {
    let (client, view, surface) = ready();
    unknown(&client);
    let ticket = capture(&client, view, &surface).unwrap();
    drop(surface);
    assert_eq!(clear(&client, &ticket), Err(Rejection::StaleSurface));
    assert!(fenced(&client));
}

#[test]
fn same_element_moving_windows_invalidates_old_ticket() {
    let (client, view, mut surface) = ready();
    unknown(&client);
    let ticket = capture(&client, view, &surface).unwrap();
    surface.bind_window(18);
    assert_eq!(clear(&client, &ticket), Err(Rejection::StaleSurface));
    assert!(fenced(&client));
}

#[test]
fn publication_replacement_rejects_even_unchanged_replica_sequence() {
    let (client, view, surface) = ready();
    unknown(&client);
    let ticket = capture(&client, view, &surface).unwrap();
    client.engine().unwrap().republish_view(view).unwrap();
    assert_eq!(clear(&client, &ticket), Err(Rejection::StalePublication));
    assert!(fenced(&client));
}

#[test]
fn scrollback_is_paintable_but_cannot_clear_terminal_recovery() {
    let (client, view, surface) = ready();
    unknown(&client);
    client.scroll_view(view, Scroll::Delta(-2)).unwrap();
    let ticket = capture(&client, view, &surface).unwrap();
    assert!(!ticket.frame.scrollbar.at_tail());
    assert_eq!(clear(&client, &ticket), Err(Rejection::NotAtTail));
    assert!(fenced(&client));
}

#[test]
fn another_live_tail_view_can_clear_the_terminal_wide_fence() {
    let (client, first_view, surface) = ready();
    let second_view = client.create_view(&terminal()).unwrap();
    unknown(&client);
    client.scroll_view(first_view, Scroll::Delta(-2)).unwrap();
    let ticket = capture(&client, second_view, &surface).unwrap();
    assert_eq!(clear(&client, &ticket), Ok(()));
    assert!(!fenced(&client));
}

#[test]
fn ordinary_unfenced_ticket_cannot_clear_a_later_unknown() {
    let (client, view, surface) = ready();
    let ticket = capture(&client, view, &surface).unwrap();
    unknown(&client);
    assert_eq!(clear(&client, &ticket), Err(Rejection::NoFence));
    assert!(fenced(&client));
}

#[test]
fn wrong_terminal_prop_is_rejected() {
    let (client, view, surface) = ready();
    let result = client.with_control(|control| {
        Ticket::capture(
            HANDLE,
            "wrong-terminal",
            view,
            17,
            &surface.lifetime,
            control,
        )
    });
    assert!(matches!(result, Err(Rejection::WrongTerminal)));
}

#[test]
fn receipt_is_bound_to_exact_window_root_and_frame() {
    let (client, view, surface) = ready();
    unknown(&client);
    let ticket = capture(&client, view, &surface).unwrap();
    // Synthetic proof only tests correlation validation, never platform presentation.
    let mut receipt = Presented {
        window: 18,
        lifetime: ticket.lifetime.clone(),
        frame: ticket.frame(),
    };
    assert_eq!(ticket.check_receipt(&receipt), Err(Rejection::WrongWindow));
    receipt.window = 17;
    let other = Surface::default();
    receipt.lifetime = Rc::downgrade(&other.lifetime);
    assert_eq!(ticket.check_receipt(&receipt), Err(Rejection::StaleSurface));
    receipt.lifetime = ticket.lifetime.clone();
    client.engine().unwrap().republish_view(view).unwrap();
    receipt.frame = client.acquire_view(view).unwrap();
    assert_eq!(
        ticket.check_receipt(&receipt),
        Err(Rejection::StalePublication)
    );
    assert!(fenced(&client));
}

#[test]
fn competing_delivery_waits_for_the_entire_conditional_clear() {
    use std::{sync::mpsc, time::Duration};

    let (client, view, surface) = ready();
    unknown(&client);
    let ticket = capture(&client, view, &surface).unwrap();
    let competitor = client.clone();
    let (start, started) = mpsc::channel();
    let (attempt, attempted) = mpsc::channel();
    let (done, completed) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        started
            .recv_timeout(Duration::from_secs(5))
            .expect("start deadline");
        attempt.send(()).unwrap();
        let fence = unknown(&competitor);
        done.send(fence).unwrap();
    });
    client.with_control(|control| {
        start.send(()).unwrap();
        attempted
            .recv_timeout(Duration::from_secs(5))
            .expect("attempt deadline");
        assert!(completed.try_recv().is_err());
        assert_eq!(ticket.acknowledge_current(control), Ok(()));
    });
    let newer = completed
        .recv_timeout(Duration::from_secs(5))
        .expect("completion deadline");
    worker.join().expect("worker");
    assert_eq!(client.projection_fence(&terminal()), Some(newer));
    assert_eq!(clear(&client, &ticket), Err(Rejection::StaleFence));
}

#[test]
fn recovery_capture_removes_prediction_without_forging_a_generation() {
    let (client, view, surface) = ready();
    unknown(&client);
    client
        .engine()
        .unwrap()
        .predict_view_text(view, "Z".into())
        .unwrap();
    let predicted = client.acquire_view(view).unwrap();
    assert!(predicted.text().ends_with("readyZ"));
    let ticket = capture(&client, view, &surface).unwrap();
    assert!(ticket.frame.text().ends_with("ready"));
    assert!(ticket.frame.generation > predicted.generation);
    assert_eq!(ticket.frame.last_seq, predicted.last_seq);
    assert!(Arc::ptr_eq(
        &ticket.frame,
        &client.acquire_view(view).unwrap()
    ));
    assert!(fenced(&client));
}

#[test]
fn bootstrap_swap_rejects_ticket_for_previous_authoritative_replica() {
    let (client, view, surface) = ready();
    unknown(&client);
    let ticket = capture(&client, view, &surface).unwrap();
    client
        .feed(FrameKind::BootstrapBegin {
            terminal_id: terminal(),
            stream_id: StreamId::new(1).unwrap(),
            bootstrap_id: BootstrapId::new(2).unwrap(),
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 20,
            rows: 4,
            base_seq: 0,
        })
        .unwrap();
    assert_eq!(client.status(), Status::Attached);
    // Bootstrap is staged: the old replica remains authoritative until Ready.
    assert_eq!(
        client
            .engine()
            .unwrap()
            .view_replica_info(view)
            .unwrap()
            .bootstrap_id,
        1
    );
    for frame in [
        FrameKind::BootstrapChunk {
            terminal_id: terminal(),
            stream_id: StreamId::new(1).unwrap(),
            bootstrap_id: BootstrapId::new(2).unwrap(),
            chunk_seq: 0,
            payload: b"replacement".to_vec().into(),
        },
        FrameKind::BootstrapReady {
            terminal_id: terminal(),
            stream_id: StreamId::new(1).unwrap(),
            bootstrap_id: BootstrapId::new(2).unwrap(),
            history_cursor: None,
        },
    ] {
        client.feed(frame).unwrap();
    }
    assert!(clear(&client, &ticket).is_err());
    let fresh = capture(&client, view, &surface).unwrap();
    assert_eq!(fresh.frame.bootstrap_id, 2);
    assert!(fresh.frame.text().contains("replacement"));
    assert!(fenced(&client));
}
