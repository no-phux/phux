//! Turning consumer requests into satellite frames: commands, content cuts, and terminal mirrors.

use super::{
    Command, CommandResult, FrameKind, PendingCommand, ProxySubscription, RelayRequest,
    RelaySession, ResourceId, Scope, ServerFeatureSet, SpawnResult, link_attach_command, oneshot,
};

impl RelaySession {
    /// Before a first explicit attach, fence any automatic SPAWN generation
    /// upstream (no proxy owns it); shared subscriptions keep streaming.
    pub(crate) fn prepare_request(&mut self, request: &RelayRequest) -> Vec<Vec<u8>> {
        if let RelayRequest::MirrorTerminal { terminal } = request {
            return self.mirror_terminal_frames(*terminal);
        }
        let RelayRequest::Command {
            command: Command::AttachResource { .. },
            subscribe: Some(sub),
            ..
        } = request
        else {
            return Vec::new();
        };
        if self.explicit_content.contains(&sub.terminal)
            || self
                .pending_detaches
                .values()
                .any(|pending| pending.terminal == sub.terminal)
            || self.subscription_rejection(sub).is_some()
        {
            return Vec::new();
        }
        self.prepare_content_cut(sub.terminal)
    }

    pub(super) fn prepare_content_cut(&mut self, terminal: u32) -> Vec<Vec<u8>> {
        let restore_events = self.legacy_events.contains(&terminal);
        let mut frames = vec![self.encode_terminal_detach(terminal)];
        if restore_events {
            self.legacy_events.insert(terminal);
            let cursor = self.link_event_cursor();
            frames.push(self.encode(&FrameKind::SubscribeEvents {
                terminal: Some(ResourceId::local(terminal)),
                after_seq: cursor,
            }));
        }
        frames
    }

    /// Service one request. `None`: rejected locally, consumer already told.
    pub(crate) fn handle_request_checked(&mut self, request: RelayRequest) -> Option<Vec<u8>> {
        match request {
            RelayRequest::Command {
                command,
                reply,
                subscribe,
            } => self.enqueue_command(command, reply, subscribe),
            RelayRequest::Keyed {
                command,
                actor,
                reply,
            } => {
                if let Some(refusal) = self
                    .command_rejection(&command)
                    .or_else(|| self.fence_rejection(&command, actor))
                {
                    let _ = reply.send(refusal);
                    return None;
                }
                let request_id = self.allocate_request_id();
                self.pending.insert(
                    request_id,
                    PendingCommand {
                        reply,
                        subscription: None,
                    },
                );
                Some(self.encode(&FrameKind::Command {
                    request_id,
                    command,
                }))
            }
            RelayRequest::Forward { frame } => Some(self.encode(&frame)),
            RelayRequest::Spawn { spawn, reply } => {
                if let Some(refusal) = self.spawn_capability_rejection(&spawn) {
                    let _ = reply.send(SpawnResult::Err(refusal));
                    return None;
                }
                let request_id = self.allocate_request_id();
                let kind = spawn
                    .resource
                    .as_ref()
                    .map_or(phux_protocol::ids::ResourceKind::Terminal, |resource| {
                        resource.kind
                    });
                self.pending_spawns.insert(request_id, (reply, kind));
                Some(self.encode(&FrameKind::SpawnResource {
                    request_id,
                    group: spawn.group,
                    command: spawn.command,
                    cwd: spawn.cwd,
                    env: spawn.env,
                    term: spawn.term,
                    // Hub-and-spoke: the satellite spawns locally.
                    satellite: None,
                    owner_terminal: spawn.owner_terminal.map(ResourceId::local),
                    agent_session: None,
                    initial_size: spawn.initial_size,
                    // Kind and retagged parent cross intact (ADR-0104 §6).
                    resource: spawn.resource,
                }))
            }
            RelayRequest::Subscribe {
                subscription,
                forward,
            } => self.subscribe_forward(subscription, &forward),
            RelayRequest::ListDirectory { path, reply } => self.enqueue_listing(path, reply),
            RelayRequest::PathQuery {
                root,
                query,
                recursive,
                reply,
            } => self.enqueue_path_query(root, query, recursive, reply),
            // Frames were produced by [`Self::prepare_request`].
            RelayRequest::MirrorTerminal { .. } => None,
        }
    }

    pub(super) fn enqueue_command(
        &mut self,
        mut command: Command,
        reply: oneshot::Sender<CommandResult>,
        subscribe: Option<ProxySubscription>,
    ) -> Option<Vec<u8>> {
        if let Some(refusal) = self.command_rejection(&command) {
            let _ = reply.send(refusal);
            return None;
        }
        if let Some(sub) = subscribe.as_ref()
            && let Some((code, message)) = self.subscription_rejection(sub)
        {
            let _ = reply.send(CommandResult::Error { code, message });
            return None;
        }
        // One upstream generation fans out to every content consumer.
        // A cursor belongs to one consumer, so a shared generation
        // must be an explicit full replacement for all of them.
        let shared_content = subscribe.as_ref().is_none_or(|sub| {
            self.subscribers
                .get(&sub.terminal)
                .is_some_and(|subscribers| {
                    subscribers
                        .iter()
                        .any(|existing| existing.client != sub.client)
                })
        });
        if shared_content
            && let Command::AttachResource {
                agent_after_seq, ..
            } = &mut command
        {
            *agent_after_seq = None;
        }
        // Register the rider with the enqueue, remembering how to roll it back.
        let subscription = subscribe.map(|sub| {
            let (terminal, client) = (sub.terminal, sub.client);
            if sub.awaits_snapshot {
                self.explicit_content.insert(terminal);
            }
            let effect = self.register_subscriber(sub);
            (terminal, client, effect)
        });
        let request_id = self.allocate_request_id();
        self.pending.insert(
            request_id,
            PendingCommand {
                reply,
                subscription,
            },
        );
        Some(self.encode(&FrameKind::Command {
            request_id,
            command: link_attach_command(
                command,
                self.satellite_features,
                self.satellite_features_ext,
            ),
        }))
    }

    /// `SUBSCRIBE_METADATA` plus a `GET_METADATA` per allowlisted key, once
    /// per terminal per connection (`METADATA_VALUE` carries no key, hence
    /// [`Self::pending_mirror_gets`]).
    pub(super) fn mirror_terminal_frames(&mut self, terminal: u32) -> Vec<Vec<u8>> {
        if !self.mirrored.insert(terminal) {
            return Vec::new();
        }
        let scope = Scope::Resource(ResourceId::local(terminal));
        let mut frames = Vec::new();
        for key in crate::hub::metadata_mirror::MIRROR_KEYS {
            frames.push(self.encode(&FrameKind::SubscribeMetadata {
                scope: scope.clone(),
                key: (*key).to_owned(),
            }));
            let request_id = self.allocate_request_id();
            self.pending_mirror_gets
                .insert(request_id, (terminal, (*key).to_owned()));
            frames.push(self.encode(&FrameKind::GetMetadata {
                request_id,
                scope: scope.clone(),
                key: (*key).to_owned(),
            }));
        }
        frames
    }

    /// The satellite's `HELLO_OK` features (ADR-0127).
    pub(crate) const fn satellite_features(&self) -> ServerFeatureSet {
        self.satellite_features
    }
}
