//! Normalized input enters the existing runtime, never a JS socket or encoder.
//! Booleans mean queued, not delivered. Durable paste uses `InputDelivery` events.

use ::napi::{Error, Result};
use napi_derive::napi;
use phux_client_runtime::{Client, control::ControlPlane};
use phux_protocol::ResourceId;
use phux_protocol::input::{
    focus::FocusEvent,
    key::{KeyAction, KeyEvent, ModSet, PhysicalKey},
    mouse::{MouseAction, MouseButton, MouseEvent},
};

use super::{
    DesktopClient,
    views::{bounded_text, finite, integer, view_terminal},
};

fn ready(control: &ControlPlane, terminal: &ResourceId) -> bool {
    if control
        .options()
        .attach_role
        .is_some_and(phux_protocol::wire::frame::RolePolicy::is_viewer)
    {
        return false;
    }
    control.input_ready(terminal)
}

fn send(
    client: &Client,
    view: &str,
    operation: impl FnOnce(&mut ControlPlane, &ResourceId) -> bool,
) -> Result<bool> {
    client.with_control(|control| {
        let terminal = view_terminal(client, control, view)?;
        if !ready(control, &terminal) {
            return Ok(false);
        }
        Ok(operation(control, &terminal))
    })
}

fn paste_delivery(client: &Client, view: &str, text: &str) -> Result<u64> {
    let admitted = client.with_control(|control| -> Result<_> {
        let terminal = view_terminal(client, control, view)?;
        if !ready(control, &terminal) {
            return Ok(None);
        }
        #[cfg(test)]
        tests::paste_checkpoint();
        let had_events = control.has_events();
        let delivery = control.apply_paste(&terminal, text);
        Ok(Some((delivery, !had_events && control.has_events())))
    })?;
    let Some((delivery, resolved)) = admitted else {
        // A refusal cannot enter the replay journal, so it is safe after unlock.
        return Ok(client.refuse_acknowledged_input("view input is not ready or is observe-only"));
    };
    // Match Client's acknowledged-input wake contract, outside the owner lock.
    if resolved {
        client.wake();
    }
    Ok(delivery)
}

fn modifiers(value: f64) -> Result<ModSet> {
    ModSet::from_bits(integer(value)?).ok_or_else(|| Error::from_reason("InvalidModifiers"))
}

fn validate_key_text(text: &str) -> Result<()> {
    bounded_text(text, 4096)?;
    if text
        .chars()
        .any(|ch| matches!(ch, '\u{0}'..='\u{1f}' | '\u{7f}' | '\u{f700}'..='\u{f8ff}'))
    {
        return Err(Error::from_reason("InvalidKeyText"));
    }
    Ok(())
}

#[napi(object)]
#[derive(Debug)]
pub struct DesktopKeyEvent {
    /// Protocol physical-key discriminant, checked rather than truncated.
    pub key: f64,
    /// 0 release, 1 press, 2 repeat.
    pub action: f64,
    pub mods: f64,
    pub consumed_mods: f64,
    pub text: Option<String>,
    pub unshifted_codepoint: Option<f64>,
    /// Marked/preedit events are rejected; only final committed input is supported.
    pub composing: Option<bool>,
}

impl DesktopKeyEvent {
    fn decode(self) -> Result<KeyEvent> {
        let (mods, consumed_mods) = key_modifiers(self.mods, self.consumed_mods)?;
        validate_key_payload(self.text.as_deref(), self.composing)?;
        Ok(KeyEvent {
            key: PhysicalKey::try_from(integer::<u32>(self.key)?)
                .map_err(|_| Error::from_reason("InvalidPhysicalKey"))?,
            action: KeyAction::from_u32(integer(self.action)?)
                .ok_or_else(|| Error::from_reason("InvalidKeyAction"))?,
            mods,
            consumed_mods,
            text: self.text,
            unshifted_codepoint: self.unshifted_codepoint.map(codepoint).transpose()?,
            composing: false,
        })
    }
}

fn validate_key_payload(text: Option<&str>, composing: Option<bool>) -> Result<()> {
    if composing == Some(true) {
        return Err(Error::from_reason("CompositionRequiresNativeImeBridge"));
    }
    if let Some(text) = text {
        validate_key_text(text)?;
    }
    Ok(())
}

fn key_modifiers(mods: f64, consumed: f64) -> Result<(ModSet, ModSet)> {
    let mods = modifiers(mods)?;
    let consumed = modifiers(consumed)?;
    if !mods.contains(consumed) {
        return Err(Error::from_reason("ConsumedModifiersNotHeld"));
    }
    Ok((mods, consumed))
}

fn codepoint(value: f64) -> Result<u32> {
    let value = integer(value)?;
    char::from_u32(value).ok_or_else(|| Error::from_reason("InvalidCodepoint"))?;
    Ok(value)
}

#[napi(object)]
#[derive(Debug)]
pub struct DesktopMouseEvent {
    /// 0 press, 1 release, 2 motion.
    pub action: f64,
    pub button: f64,
    pub mods: f64,
    /// Pane-local surface-space pixels, not cells.
    pub x: f64,
    pub y: f64,
}

impl DesktopMouseEvent {
    fn decode(self) -> Result<MouseEvent> {
        Ok(MouseEvent {
            action: MouseAction::try_from(integer::<u32>(self.action)?)
                .map_err(|_| Error::from_reason("InvalidMouseAction"))?,
            button: MouseButton::try_from(integer::<u32>(self.button)?)
                .map_err(|_| Error::from_reason("InvalidMouseButton"))?,
            mods: modifiers(self.mods)?,
            x: finite(self.x)?,
            y: finite(self.y)?,
        })
    }
}

#[napi]
#[allow(
    clippy::needless_pass_by_value,
    reason = "NAPI requires owned JS values"
)]
impl DesktopClient {
    /// Committed text only; marked/preedit text stays in the native IME bridge.
    /// Named/control keys use keyEvent, clipboard content uses pasteView.
    #[napi]
    pub fn commit_text(&self, view: String, text: String) -> Result<bool> {
        validate_key_text(&text)?;
        send(&self.client()?, &view, |control, id| {
            control.send_text(id, &text)
        })
    }

    #[napi]
    pub fn key_event(&self, view: String, event: DesktopKeyEvent) -> Result<bool> {
        let event = event.decode()?;
        send(&self.client()?, &view, |control, id| {
            control.send_key(id, event)
        })
    }

    #[napi]
    pub fn mouse_event(&self, view: String, event: DesktopMouseEvent) -> Result<bool> {
        let event = event.decode()?;
        send(&self.client()?, &view, |control, id| {
            control.send_mouse(id, event)
        })
    }

    #[napi]
    pub fn focus_view(&self, view: String, focused: bool) -> Result<bool> {
        let event = if focused {
            FocusEvent::Gained
        } else {
            FocusEvent::Lost
        };
        send(&self.client()?, &view, |control, id| {
            control.send_focus(id, event)
        })
    }

    /// Returns a correlation, never a delivery claim. Read the shared typed
    /// `InputDelivery` event (Delivered/Refused/Unknown); do not retry Unknown.
    #[napi]
    pub fn paste_view(&self, view: String, text: String) -> Result<String> {
        let client = self.client()?;
        Ok(paste_delivery(&client, &view, &text)?.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    use phux_client_runtime::control::{ControlOptions, DeliveryOutcome, Event};
    use phux_client_runtime::{Listener, Runtime};
    use phux_protocol::PROTOCOL_VERSION;
    use phux_protocol::caps::{
        BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, ServerCapabilities,
        ServerFeature, ServerFeatureSet,
    };
    use phux_protocol::ids::{BootstrapId, ClientId, SessionId, StreamId, WindowId};
    use phux_protocol::wire::frame::{AttachTarget, FrameKind, RolePolicy};
    use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};

    use super::super::views::{DesktopResizeOutcome, resize_view, view_id};
    use super::*;

    thread_local! {
        // Force the disconnect race precisely after the readiness check. This
        // checkpoint is absent from production and isolated to the test thread.
        static PASTE_CHECKPOINT: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
    }

    pub(super) fn paste_checkpoint() {
        PASTE_CHECKPOINT.with_borrow_mut(|slot| {
            if let Some(checkpoint) = slot.take() {
                checkpoint();
            }
        });
    }

    fn ready_client() -> (Client, String) {
        ready_client_with_role(None)
    }

    fn ready_client_with_role(attach_role: Option<RolePolicy>) -> (Client, String) {
        let client = Runtime::embedded(ControlOptions {
            attach: Some(AttachTarget::ByName("input-test".into())),
            attach_role,
            ..ControlOptions::default()
        });
        bootstrap(&client, BootstrapLimits::default());
        let view = client
            .create_view(&ResourceId::local(7))
            .expect("create view");
        (client, view.get().to_string())
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
            .expect("attach requested");
        finish_bootstrap(client, attach_id);
        assert!(client.input_ready(&ResourceId::local(7)));
        let _ = client.take_outbound();
        let _ = client.take_events();
    }

    fn finish_bootstrap(client: &Client, attach_id: u32) {
        let terminal_id = ResourceId::local(7);
        let stream_id = StreamId::new(1).expect("stream");
        let bootstrap_id = BootstrapId::new(1).expect("bootstrap");
        let snapshot =
            SessionSnapshot::new(SessionId::new(1), WindowId::new(1), terminal_id.clone())
                .with_sessions(vec![SessionInfo::new(SessionId::new(1), "input-test")])
                .with_windows(vec![WindowInfo::new(
                    WindowId::new(1),
                    SessionId::new(1),
                    "shell",
                )])
                .with_resources(vec![ResourceInfo::new(
                    terminal_id.clone(),
                    WindowId::new(1),
                    20,
                    4,
                )]);
        for frame in [
            FrameKind::Attached {
                attach_id,
                snapshot,
                initial_client_id: ClientId::new(1),
            },
            FrameKind::BootstrapBegin {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                profile: BootstrapStreamProfile::SynthesizedVtRaw,
                cols: 20,
                rows: 4,
                base_seq: 0,
            },
            FrameKind::BootstrapChunk {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                chunk_seq: 0,
                payload: b"ready".to_vec().into(),
            },
            FrameKind::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor: None,
            },
            FrameKind::AttachReady { attach_id },
        ] {
            client.feed(frame).expect("bootstrap frame");
        }
    }

    #[test]
    fn disconnect_cannot_interleave_readiness_and_replay_admission() {
        let (client, view) = ready_client();
        let (start, started) = mpsc::channel();
        let (attempt, attempted) = mpsc::channel();
        let (done, completed) = mpsc::channel();
        let competing = client.clone();
        let disconnect = std::thread::spawn(move || {
            started.recv().expect("checkpoint reached");
            attempt.send(()).expect("announce lock attempt");
            competing.with_control(|control| {
                assert!(
                    control.next_input_deadline().is_some(),
                    "admission precedes disconnect"
                );
                control.connection_lost(None);
            });
            let _ = done.send(());
        });
        PASTE_CHECKPOINT.with_borrow_mut(|slot| {
            *slot = Some(Box::new(move || {
                start.send(()).expect("start disconnect");
                attempted.recv().expect("disconnect attempts control lock");
                assert!(
                    matches!(
                        completed.recv_timeout(Duration::from_millis(100)),
                        Err(mpsc::RecvTimeoutError::Timeout)
                    ),
                    "disconnect must remain blocked between ready and journal admission"
                );
            }));
        });
        let accepted = paste_delivery(&client, &view, "before disconnect").expect("admitted");
        disconnect.join().expect("disconnect thread");
        let _ = client.take_outbound();
        let refused = paste_delivery(&client, &view, "must never replay").expect("local refusal");
        assert_ne!(accepted, refused);
        assert_refused(&client, refused);
        assert!(client.take_outbound().is_empty());
    }

    #[test]
    fn replaced_owner_rejects_old_view_even_when_terminal_id_is_reused() {
        let (client, old_view) = ready_client();
        let old_frame = client
            .acquire_view(view_id(&old_view).expect("view id"))
            .expect("frame");
        bootstrap(
            &client,
            BootstrapLimits::new(2048, 2048).expect("new negotiated limits"),
        );
        assert!(
            client.input_ready(&old_frame.terminal_id),
            "same terminal id is ready again"
        );
        assert!(
            send(&client, &old_view, |control, id| control
                .send_text(id, "stale"))
            .is_err()
        );
        assert!(paste_delivery(&client, &old_view, "stale paste").is_err());
        assert!(resize_view(&client, &old_view, 40.0, 8.0).is_err());
        assert!(client.take_outbound().is_empty());
        assert!(client.with_control(|control| control.next_input_deadline().is_none()));
        let new_view = client
            .create_view(&old_frame.terminal_id)
            .expect("replacement view")
            .get()
            .to_string();
        assert!(
            send(&client, &new_view, |control, id| control
                .send_text(id, "current"))
            .expect("new view")
        );
        assert!(!client.take_outbound().is_empty());
    }

    #[derive(Default)]
    struct Wakes(AtomicUsize);

    impl Listener for Wakes {
        fn on_activity(&self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn assert_refused(client: &Client, expected: u64) {
        assert!(client.take_events().iter().any(|event| matches!(event,
            Event::InputDelivery { delivery_id, outcome: DeliveryOutcome::Refused, .. }
                if *delivery_id == expected)));
    }

    #[test]
    fn immediate_paste_receipts_wake_the_existing_listener() {
        let (client, view) = ready_client();
        let wakes = Arc::new(Wakes::default());
        client.set_listener(wakes.clone());
        let _ = client.take_events();
        let before = wakes.0.load(Ordering::SeqCst);
        let oversized =
            paste_delivery(&client, &view, &"x".repeat(65536)).expect("oversized refusal");
        assert_eq!(wakes.0.load(Ordering::SeqCst), before + 1);
        assert_refused(&client, oversized);
        assert!(client.take_outbound().is_empty());
        client.with_control(|control| control.connection_lost(None));
        let _ = client.take_events();
        let refused = paste_delivery(&client, &view, "disconnected").expect("not ready refusal");
        assert_eq!(wakes.0.load(Ordering::SeqCst), before + 2);
        assert_refused(&client, refused);
        assert!(client.with_control(|control| control.next_input_deadline().is_none()));
    }

    #[test]
    fn resize_reports_queue_without_optimistic_geometry_and_refuses_observers() {
        let (client, view) = ready_client();
        let frame = client
            .acquire_view(view_id(&view).expect("id"))
            .expect("frame");
        assert_eq!(
            resize_view(&client, &view, 40.0, 8.0).expect("request"),
            DesktopResizeOutcome::Queued
        );
        let outbound = client.take_outbound();
        assert_eq!(outbound.len(), 1);
        assert!(matches!(FrameKind::decode(&outbound[0]).expect("decode").0,
            FrameKind::ResizeTerminal { terminal_id, cols: 40, rows: 8 } if terminal_id == frame.terminal_id));
        let after = client
            .acquire_view(view_id(&view).expect("id"))
            .expect("frame");
        assert_eq!(
            (after.cols, after.rows, after.generation),
            (frame.cols, frame.rows, frame.generation)
        );
        client.with_control(|control| control.connection_lost(None));
        assert_eq!(
            resize_view(&client, &view, 40.0, 8.0).expect("not ready"),
            DesktopResizeOutcome::NotReady
        );
        assert!(client.take_outbound().is_empty());

        let (observer, view) = ready_client_with_role(Some(RolePolicy::VIEWER));
        assert_eq!(
            resize_view(&observer, &view, 40.0, 8.0).expect("observer"),
            DesktopResizeOutcome::Observer
        );
        assert!(observer.take_outbound().is_empty());
    }

    #[test]
    fn rejects_reserved_modifier_bits_and_invalid_scalars() {
        for value in [1024.0, 65536.0, -1.0, 1.5, f64::NAN] {
            assert!(modifiers(value).is_err());
        }
        for value in [55296.0, 1_114_112.0, -1.0, 1.5] {
            assert!(codepoint(value).is_err());
        }
        assert!(codepoint(128_578.0).is_ok());
        assert!(validate_key_text("é🙂").is_ok());
        assert!(validate_key_text("\n").is_err());
        assert!(validate_key_text("\u{f700}").is_err());
    }
}
