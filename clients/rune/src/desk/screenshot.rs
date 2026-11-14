//! A screenshot sent to a terminal: the system's region picker takes it,
//! then it goes the way a pasted image does, uploaded to the terminal's host
//! and its path typed at the prompt, for the agent in that pane to read.

use std::sync::{Arc, Mutex};

use phux_client_runtime::control::PrivateUploadDestination;
use phux_protocol::ids::ResourceId;

use super::{Desk, NoticeKind};
use crate::screenshot::{self, Shot};

/// A capture under way: who it is for, and where its worker leaves it.
pub struct Capture {
    terminal: ResourceId,
    /// The connection it was asked on; a move to another machine voids it.
    server: u64,
    destination: PrivateUploadDestination,
    done: Arc<Mutex<Option<Shot>>>,
    picker: screenshot::Picker,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.picker.cancel();
        if let Some(worker) = self.worker.take() {
            // The worker owns private files. Reap it before window/app exit,
            // after cancelling only its recorded picker child.
            let _ = worker.join();
        }
    }
}

impl Desk {
    /// Closing a keep-running window hides it without dropping its Desk.
    pub(crate) fn cancel_screenshot(&mut self) {
        self.capture = None;
    }

    /// Pick a region of the screen for the terminal with the keyboard now.
    pub(super) fn screenshot(&mut self) {
        if self.capture.is_some() {
            return self.say(NoticeKind::Info, "A screenshot is already being taken.");
        }
        let Some((terminal, view)) = self.keyboard() else {
            return self.say(NoticeKind::Error, "No terminal to send a screenshot to.");
        };
        if view.is_none() || self.is_document(terminal) || !self.pastes_allowed() {
            return;
        }
        let destination = self.client.as_ref().and_then(|client| {
            client.with_control(|control| control.private_upload_destination(terminal))
        });
        let Some(destination) = destination else {
            return self.say(NoticeKind::Error,
                "Screenshot not taken: this terminal's live execution cannot be verified. Reconnect to its host directly; older servers and satellite panes aren't supported.");
        };
        if let Some(why) = screenshot::refusal() {
            return self.say(NoticeKind::Error, why);
        }
        let terminal = terminal.clone();
        let done: Arc<Mutex<Option<Shot>>> = Arc::default();
        let (slot, waker) = (Arc::clone(&done), self.waker.clone());
        let picker = screenshot::Picker::default();
        let worker_picker = picker.clone();
        let worker = std::thread::spawn(move || {
            let shot = screenshot::take(&worker_picker);
            if let Ok(mut slot) = slot.lock() {
                *slot = Some(shot);
            }
            if let Some(waker) = waker {
                waker();
            }
        });
        self.capture = Some(Capture {
            terminal,
            server: self.link.server,
            destination,
            done,
            picker,
            worker: Some(worker),
        });
    }

    /// The picker closing. True when the user should hear about it.
    pub(super) fn finish_screenshot(&mut self) -> bool {
        let Some(capture) = &self.capture else {
            return false;
        };
        let Some(shot) = capture.done.lock().ok().and_then(|mut slot| slot.take()) else {
            return false;
        };
        let Some(capture) = self.capture.take() else {
            return false;
        };
        match shot {
            Shot::Cancelled => return false,
            Shot::Failed(why) => {
                self.say(NoticeKind::Error, format!("Screenshot not taken: {why}"))
            }
            Shot::Taken(png) => self.send_screenshot(&capture, png),
        }
        true
    }

    /// To the terminal it was taken for, wherever the keyboard went since.
    fn send_screenshot(&mut self, capture: &Capture, png: Vec<u8>) {
        let refusal = if capture.server != self.link.server {
            Some("This window moved to another machine; the screenshot wasn't sent.")
        } else if !self.pastes_allowed() {
            Some("Another field or question has the keyboard; the screenshot wasn't sent.")
        } else if self.client.is_none() {
            Some("Not connected; the screenshot wasn't sent.")
        } else if self.topology.is_some() && !self.lists(&capture.terminal) {
            Some("That terminal closed; the screenshot wasn't sent.")
        } else {
            None
        };
        match refusal {
            Some(why) => self.say(NoticeKind::Error, why),
            None => {
                let Some(client) = &self.client else { return };
                let transfer = client
                    .with_control(|control| control.put_private_image(&capture.destination, png));
                self.uploads.push((
                    transfer,
                    capture.terminal.clone(),
                    Some(capture.destination.clone()),
                ));
                self.say(NoticeKind::Info, "Uploading image…");
            }
        }
    }

    /// Check and paste while holding the runtime's one control lock. A
    /// completed upload is not permission to deliver into a replacement run.
    pub(super) fn deliver_private_image(
        &mut self,
        destination: &PrivateUploadDestination,
        line: &str,
    ) -> bool {
        if !self.pastes_allowed() {
            return false;
        }
        let Some(client) = &self.client else {
            return false;
        };
        let sent =
            client.with_control(|control| control.paste_private_image_path(destination, line));
        if sent {
            self.reclaim_fits();
        }
        sent
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::Resolved;
    use phux_client_runtime::control::{ControlOptions, ControlPlane};
    use phux_protocol::{
        PROTOCOL_VERSION,
        caps::{
            BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, ServerCapabilities,
            ServerFeature, ServerFeatureExt, ServerFeatureExtSet, ServerFeatureSet,
        },
        ids::{
            ActivationId, AuthorityId, BootstrapId, ClientId, ResourceKey, Revision,
            ServerInstance, SessionId, SessionKey, StreamId, WindowId,
        },
        wire::{
            frame::{
                ActivationInfo, AttachTarget, Command, CommandResult, CommandValue, ExecState,
                FrameKind, OrganizationPage, ResourceRecord,
            },
            info::{ResourceInfo, SessionSnapshot},
        },
    };

    fn desk() -> Desk {
        Desk::new(&Resolved::offline("not launched"))
    }

    fn connected(desk: &mut Desk) {
        let client = Arc::new(phux_client_runtime::Runtime::embedded(ControlOptions {
            attach: Some(AttachTarget::ByName("work".into())),
            viewport: (80, 24),
            ..ControlOptions::default()
        }));
        client.with_control(ControlPlane::connection_opened);
        let _ = client.take_outbound();
        client
            .feed(FrameKind::HelloOk {
                protocol_major: PROTOCOL_VERSION.major,
                protocol_minor: PROTOCOL_VERSION.minor,
                protocol_patch: PROTOCOL_VERSION.patch,
                server_caps: ServerCapabilities::new()
                    .with_features(ServerFeatureSet::with(&[
                        ServerFeature::FileUpload,
                        ServerFeature::AcknowledgedInput,
                    ]))
                    .with_features_ext(ServerFeatureExtSet::with(&[
                        ServerFeatureExt::Organization,
                    ])),
                server_id: vec![1; 16],
                selected_profile: Some(BootstrapProfile::SynthesizedVtRaw),
                bootstrap_limits: BootstrapLimits::default(),
            })
            .unwrap();
        let frames: Vec<_> = client
            .take_outbound()
            .iter()
            .map(|bytes| FrameKind::decode(bytes).unwrap().0)
            .collect();
        let request = frames
            .iter()
            .find_map(|frame| match frame {
                FrameKind::Command {
                    request_id,
                    command: Command::GetOrganization { .. },
                } => Some(*request_id),
                _ => None,
            })
            .unwrap();
        let attach = frames
            .iter()
            .find_map(|frame| match frame {
                FrameKind::Attach { attach_id, .. } => Some(*attach_id),
                _ => None,
            })
            .unwrap();
        let session = SessionKey::new([1; 16]).unwrap();
        let records = [1, 3]
            .into_iter()
            .map(|n| {
                ResourceRecord::new(
                    ResourceKey::new([n; 16]).unwrap(),
                    session,
                    Revision::new(1),
                )
                .with_live(Some(ResourceId::local(u32::from(n))))
                .with_activation(Some(ActivationInfo::new(
                    ActivationId::new(1),
                    ServerInstance::new([1; 16]),
                    ExecState::Running,
                )))
            })
            .collect();
        client
            .feed(FrameKind::CommandResult {
                request_id: request,
                result: CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(
                    OrganizationPage::new(AuthorityId::new([1; 16]).unwrap(), Revision::new(1), 0)
                        .with_resources(records),
                ))),
            })
            .unwrap();
        client
            .feed(FrameKind::Attached {
                attach_id: attach,
                snapshot: SessionSnapshot::new(
                    SessionId::new(1),
                    WindowId::new(1),
                    ResourceId::local(1),
                )
                .with_resources(
                    [1, 3]
                        .into_iter()
                        .map(|n| {
                            ResourceInfo::new(ResourceId::local(n), WindowId::new(1), 80, 24)
                                .with_session(Some(SessionId::new(1)))
                        })
                        .collect(),
                ),
                initial_client_id: ClientId::new(1),
            })
            .unwrap();
        for n in [1, 3] {
            bootstrap(&client, n);
        }
        client
            .feed(FrameKind::AttachReady { attach_id: attach })
            .unwrap();
        desk.client = Some(client);
    }

    fn bootstrap(client: &phux_client_runtime::Client, n: u32) {
        let stream_id = StreamId::new(u64::from(n)).unwrap();
        let bootstrap_id = BootstrapId::new(1).unwrap();
        for frame in [
            FrameKind::BootstrapBegin {
                terminal_id: ResourceId::local(n),
                stream_id,
                bootstrap_id,
                profile: BootstrapStreamProfile::SynthesizedVtRaw,
                cols: 80,
                rows: 24,
                base_seq: 0,
            },
            FrameKind::BootstrapChunk {
                terminal_id: ResourceId::local(n),
                stream_id,
                bootstrap_id,
                chunk_seq: 0,
                payload: b"test fixture".to_vec().into(),
            },
            FrameKind::BootstrapReady {
                terminal_id: ResourceId::local(n),
                stream_id,
                bootstrap_id,
                history_cursor: None,
            },
        ] {
            client.feed(frame).unwrap();
        }
    }

    fn taken(desk: &mut Desk, terminal: u32, shot: Shot) {
        if desk.client.is_none() {
            connected(desk);
        }
        let destination = desk.client.as_ref().unwrap().with_control(|control| {
            control
                .private_upload_destination(&ResourceId::local(terminal))
                .unwrap()
        });
        desk.capture = Some(Capture {
            terminal: ResourceId::local(terminal),
            server: desk.link.server,
            destination,
            done: Arc::new(Mutex::new(Some(shot))),
            picker: screenshot::Picker::default(),
            worker: None,
        });
    }

    #[test]
    fn a_screenshot_uploads_for_the_terminal_it_was_taken_for() {
        let mut desk = desk();
        connected(&mut desk);
        taken(&mut desk, 3, Shot::Taken(b"\x89PNG".to_vec()));
        assert!(desk.finish_screenshot());
        assert!(desk.capture.is_none());
        assert_eq!(desk.uploads.len(), 1);
        assert_eq!(desk.uploads[0].1, ResourceId::local(3));
    }

    #[test]
    fn a_cancel_says_nothing() {
        let mut desk = desk();
        connected(&mut desk);
        taken(&mut desk, 1, Shot::Cancelled);
        assert!(!desk.finish_screenshot());
        assert!(desk.capture.is_none(), "the next one may start");
        assert!(desk.notice.is_none());
        assert!(desk.uploads.is_empty());
    }

    #[test]
    fn a_failure_is_heard() {
        let mut desk = desk();
        taken(&mut desk, 1, Shot::Failed("no picker".into()));
        assert!(desk.finish_screenshot());
        assert!(desk.notice.is_some());
    }

    #[test]
    fn a_screenshot_never_follows_the_window_to_another_server() {
        let mut desk = desk();
        connected(&mut desk);
        taken(&mut desk, 1, Shot::Taken(b"\x89PNG".to_vec()));
        desk.left_server();
        assert!(desk.finish_screenshot());
        assert!(desk.uploads.is_empty());
        assert!(desk.notice.is_some(), "the person hears why");
    }

    #[test]
    fn an_unfinished_capture_waits() {
        let mut desk = desk();
        taken(&mut desk, 1, Shot::Cancelled);
        *desk.capture.as_ref().unwrap().done.lock().unwrap() = None;
        assert!(!desk.finish_screenshot());
        assert!(desk.capture.is_some());
        desk.screenshot();
        assert!(desk.notice.is_some(), "one at a time, and said so");
    }

    #[test]
    fn no_terminal_no_picker() {
        let mut desk = desk();
        desk.screenshot();
        assert!(desk.capture.is_none());
        assert!(desk.notice.is_some());
    }

    #[test]
    fn closing_a_keep_running_window_releases_its_capture_owner() {
        let mut desk = desk();
        taken(&mut desk, 1, Shot::Cancelled);
        desk.cancel_screenshot();
        assert!(desk.capture.is_none());
        assert!(desk.uploads.is_empty());
    }

    #[test]
    fn a_capture_finishing_after_disconnect_never_queues_private_bytes() {
        let mut desk = desk();
        taken(&mut desk, 1, Shot::Taken(vec![7]));
        let client = desk.client.clone().unwrap();
        client.with_control(|control| control.connection_lost(None));
        let _ = client.take_outbound();
        assert!(desk.finish_screenshot());
        assert!(client.take_outbound().is_empty());
        let receipts = client.take_file_upload_receipts();
        assert_eq!(receipts.len(), 1);
        assert_eq!(
            receipts[0].outcome,
            phux_client_runtime::control::FileUploadOutcome::Refused
        );
    }

    #[test]
    fn delayed_private_path_delivery_never_targets_another_connection() {
        let mut desk = desk();
        taken(&mut desk, 1, Shot::Cancelled);
        let capture = desk.capture.take().unwrap();
        let client = desk.client.clone().unwrap();
        client.with_control(|control| {
            control.connection_lost(None);
            control.connection_opened();
        });
        let _ = client.take_outbound();
        assert!(!desk.deliver_private_image(&capture.destination, "/private/image.png "));
        assert!(client.take_outbound().is_empty());
        assert!(desk.pastes.is_empty());
    }

    #[test]
    fn a_completed_upload_receipt_cannot_paste_after_the_connection_changes() {
        use phux_protocol::wire::frame::FileUploadAck;

        let mut desk = desk();
        taken(&mut desk, 1, Shot::Taken(vec![7]));
        let client = desk.client.clone().unwrap();
        let _ = client.take_outbound();
        assert!(desk.finish_screenshot());
        let request = client
            .take_outbound()
            .iter()
            .find_map(|bytes| match FrameKind::decode(bytes).unwrap().0 {
                FrameKind::Command {
                    request_id,
                    command: Command::PutFile { .. },
                } => Some(request_id),
                _ => None,
            })
            .unwrap();
        client
            .feed(FrameKind::CommandResult {
                request_id: request,
                result: CommandResult::OkWith(CommandValue::FileUpload(FileUploadAck {
                    next_offset: 1,
                    path: Some("/private/image.png".into()),
                })),
            })
            .unwrap();
        client.with_control(|control| {
            control.connection_lost(None);
            control.connection_opened();
        });
        let _ = client.take_outbound();
        assert!(desk.finish_uploads(&client));
        assert!(client.take_outbound().is_empty());
        assert!(desk.uploads.is_empty());
        assert!(desk.pastes.is_empty());
        assert!(desk.notice.as_ref().unwrap().text.contains("not pasted"));
    }

    #[test]
    fn a_success_types_once_to_the_original_pane_without_focus_or_replay() {
        let mut desk = desk();
        taken(&mut desk, 1, Shot::Cancelled);
        let capture = desk.capture.take().unwrap();
        let client = desk.client.clone().unwrap();
        let _ = client.take_outbound();
        assert!(client.input_ready(&ResourceId::local(1)));
        assert!(desk.deliver_private_image(&capture.destination, "/private/image.png "));
        let frames: Vec<_> = client
            .take_outbound()
            .iter()
            .map(|bytes| FrameKind::decode(bytes).unwrap().0)
            .collect();
        assert!(
            matches!(&frames[..], [FrameKind::InputPaste { terminal_id, .. }] if terminal_id == &ResourceId::local(1))
        );
        assert!(desk.shown.is_none(), "delivery never takes focus");
        assert!(
            desk.pastes.is_empty(),
            "no private path in reconnect journal"
        );
        client.with_control(|control| {
            control.connection_lost(None);
            control.connection_opened();
            control.expire_operations();
        });
        assert!(client.take_outbound().iter().all(|bytes| !matches!(
            FrameKind::decode(bytes).unwrap().0,
            FrameKind::InputPaste { .. }
                | FrameKind::Command {
                    command: Command::ApplyInput { .. },
                    ..
                }
        )));
    }
}
