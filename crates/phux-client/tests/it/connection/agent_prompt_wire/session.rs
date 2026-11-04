use super::*;

#[allow(
    clippy::too_many_lines,
    reason = "one server, one match; splitting it hides the frame ordering"
)]
pub(super) async fn session(stream: UnixStream, script: Script, seen: SeenIds) {
    let trace = script.trace.clone();
    let mut link = Link {
        stream,
        out: BytesMut::new(),
    };
    let mut applies = 0_usize;
    while let Some(frame) = link.recv().await {
        match frame {
            FrameKind::Hello { client_caps, .. } => {
                note(trace.as_ref(), Observed::Hello);
                let (selected_profile, bootstrap_limits) =
                    select_bootstrap_profile(&client_caps, &BootstrapCapabilities::new())
                        .expect("shared bootstrap profile");
                let features = ServerFeatureSet::from_wire(ServerFeature::AcknowledgedInput as u32);
                link.send(&FrameKind::HelloOk {
                    protocol_major: PROTOCOL_VERSION.major,
                    protocol_minor: PROTOCOL_VERSION.minor,
                    protocol_patch: PROTOCOL_VERSION.patch,
                    server_caps: ServerCapabilities::new().with_features(features),
                    server_id: Vec::new(),
                    selected_profile: Some(selected_profile),
                    bootstrap_limits,
                })
                .await;
            }
            FrameKind::GetMetadata {
                request_id, key, ..
            } => {
                note(trace.as_ref(), Observed::GetMetadata { request_id });
                let value = (key == RESOURCE_AGENT_KEY)
                    .then(|| script.record.clone())
                    .flatten();
                link.send(&FrameKind::MetadataValue { request_id, value })
                    .await;
            }
            FrameKind::Command {
                request_id,
                command:
                    Command::ApplyInput {
                        operation_id,
                        terminal_id,
                        events,
                    },
            } => {
                note(
                    trace.as_ref(),
                    Observed::ApplyInput {
                        request_id,
                        operation_id,
                        events: events.len(),
                    },
                );
                seen.lock().expect("ids lock").push(operation_id);
                let result = script
                    .apply
                    .get(applies)
                    .or_else(|| script.apply.last())
                    .cloned()
                    .unwrap_or(CommandResult::Ok);
                applies = applies.saturating_add(1);
                link.send(&FrameKind::CommandResult { request_id, result })
                    .await;
                for value in script.post_result.clone() {
                    link.send(&FrameKind::MetadataChanged {
                        scope: Scope::Resource(terminal_id.clone()),
                        key: RESOURCE_AGENT_KEY.to_owned(),
                        value,
                        actor: None,
                    })
                    .await;
                }
            }
            // Subscriptions get no reply, like the real server.
            FrameKind::Command { request_id, .. } => {
                note(trace.as_ref(), Observed::Other);
                link.send(&FrameKind::CommandResult {
                    request_id,
                    result: CommandResult::Ok,
                })
                .await;
            }
            FrameKind::SubscribeEvents { .. } => note(trace.as_ref(), Observed::SubscribeEvents),
            FrameKind::SubscribeMetadata { .. } => {
                note(trace.as_ref(), Observed::SubscribeMetadata);
            }
            _ => note(trace.as_ref(), Observed::Other),
        }
    }
}
