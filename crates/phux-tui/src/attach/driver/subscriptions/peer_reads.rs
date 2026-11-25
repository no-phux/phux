//! Correlated peer/sidebar reads and their refusal, expiry and subscription
//! lifetimes. The enclosing watch retains the observed caches and projection.

use phux_protocol::ids::{ResourceId, SessionId};

use super::{AttachError, Connection, FrameKind, PeerWatch, Scope};

/// Global facts are the only reads the deferred sidebar sweep can request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::attach::driver) enum SidebarFact {
    ServingHost,
    ProjectTag,
    SessionOrder,
}

/// What a metadata read was asking for, not what shape its bytes resemble.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::attach::driver) enum PeerRead {
    Fact(SidebarFact),
    Layout(SessionId),
    Agent(ResourceId),
    Asked(ResourceId),
}

pub(in crate::attach::driver) enum PeerReply {
    Value(PeerRead, Option<Vec<u8>>),
    Refused,
}

pub(super) struct InventoryRead {
    request_id: u32,
    since: std::time::Instant,
    held: Vec<String>,
}

#[allow(
    clippy::large_enum_variant,
    reason = "move the wire result to its immediate fold without a heap allocation"
)]
pub(in crate::attach::driver) enum InventoryReply {
    Completed {
        result: phux_protocol::wire::frame::CommandResult,
        asked_at: std::time::Instant,
        held: Vec<String>,
    },
    Refused(Vec<String>),
    HeldUnreachable,
}

impl PeerWatch {
    /// Global subscriptions share this entry's send-once lifetime.
    pub(in crate::attach::driver) async fn subscribe_global_session_keys(
        &mut self,
        conn: &mut Connection,
        organization: bool,
    ) -> Result<(), AttachError> {
        if self.global_subscribed {
            return Ok(());
        }
        use phux_protocol::wire::frame::{
            CONFIG_RELOAD_KEY, SESSION_KEEP_EMPTY_KEY, SESSION_NAME_KEY, SESSION_PROJECT_KEY,
        };
        let legacy: &[&str] = if organization {
            &[]
        } else {
            &[SESSION_KEEP_EMPTY_KEY, SESSION_NAME_KEY]
        };
        for key in [
            CONFIG_RELOAD_KEY,
            SESSION_PROJECT_KEY,
            phux_client::session_order::SESSION_ORDER_KEY,
        ]
        .into_iter()
        .chain(legacy.iter().copied())
        {
            conn.send(&FrameKind::SubscribeMetadata {
                scope: Scope::Global,
                key: key.to_owned(),
            })
            .await?;
        }
        self.global_subscribed = true;
        Ok(())
    }

    pub(in crate::attach::driver) fn inventory_in_flight(&self) -> bool {
        self.inventory.is_some()
    }

    pub(in crate::attach::driver) async fn request_inventory(
        &mut self,
        conn: &mut Connection,
        next_request_id: &mut u32,
    ) -> Result<(), AttachError> {
        if self.inventory.is_some() {
            return Ok(());
        }
        let request_id = *next_request_id;
        *next_request_id = next_request_id.wrapping_add(1);
        self.inventory = Some(InventoryRead {
            request_id,
            since: std::time::Instant::now(),
            held: Vec::new(),
        });
        crate::attach::driver::session_io::send_unless_peer_gone(
            conn,
            &FrameKind::Command {
                request_id,
                command: phux_protocol::wire::frame::Command::GetState {
                    scope: phux_protocol::wire::frame::StateScope::Server,
                },
            },
        )
        .await
    }

    /// Consume only this inventory's reply/refusal. Uncorrelated unreachable
    /// pushes wait for its explanation; other errors keep their own owner.
    pub(in crate::attach::driver) fn take_inventory_reply(
        &mut self,
        frame: FrameKind,
    ) -> Result<InventoryReply, FrameKind> {
        let Some(pending) = self.inventory.as_mut() else {
            return Err(frame);
        };
        match frame {
            FrameKind::CommandResult { request_id, result } if request_id == pending.request_id => {
                let Some(read) = self.inventory.take() else {
                    unreachable!("matched inventory read")
                };
                Ok(InventoryReply::Completed {
                    result,
                    asked_at: read.since,
                    held: read.held,
                })
            }
            FrameKind::Error {
                request_id: Some(request_id),
                ..
            } if request_id == pending.request_id => {
                let Some(read) = self.inventory.take() else {
                    unreachable!("matched inventory refusal")
                };
                Ok(InventoryReply::Refused(read.held))
            }
            FrameKind::Error {
                request_id: None,
                code: phux_protocol::wire::frame::ErrorCode::SatelliteUnreachable,
                message,
            } => {
                pending.held.push(message);
                Ok(InventoryReply::HeldUnreachable)
            }
            other => Err(other),
        }
    }

    /// Deadline expiry retires the correlation and releases every held notice.
    pub(in crate::attach::driver) fn expire_inventory(&mut self, now: std::time::Instant) -> Option<Vec<String>> {
        let since = self.inventory.as_ref().map(|read| read.since);
        if !crate::attach::driver::loop_state::host_inventory_overdue(since, now) {
            return None;
        }
        self.inventory.take().map(|read| read.held)
    }

    /// Queue a global sidebar fact at most once while its GET is in flight.
    /// The serving identity is attempted once per connection, even on refusal.
    pub(in crate::attach::driver) async fn request_fact(
        &mut self,
        conn: &mut Connection,
        next_request_id: &mut u32,
        fact: SidebarFact,
    ) -> Result<(), AttachError> {
        if fact == SidebarFact::ServingHost && self.serving_host_attempted {
            return Ok(());
        }
        let read = PeerRead::Fact(fact);
        if self.pending.values().any(|pending| pending == &read) {
            return Ok(());
        }
        let key = match fact {
            SidebarFact::ServingHost => phux_protocol::wire::frame::WHOAMI_KEY,
            SidebarFact::ProjectTag => phux_protocol::wire::frame::SESSION_PROJECT_KEY,
            SidebarFact::SessionOrder => phux_client::session_order::SESSION_ORDER_KEY,
        };
        let request_id = *next_request_id;
        *next_request_id = next_request_id.wrapping_add(1);
        self.serving_host_attempted |= fact == SidebarFact::ServingHost;
        self.pending.insert(request_id, read);
        crate::attach::driver::session_io::send_unless_peer_gone(
            conn,
            &FrameKind::GetMetadata {
                request_id,
                scope: Scope::Global,
                key: key.to_owned(),
            },
        )
        .await
    }

    /// Only matched replies consume correlation. Refusals retire the same
    /// pending read as values, without clearing its previously observed cache.
    pub(in crate::attach::driver) fn take_metadata_reply(&mut self, frame: FrameKind) -> Result<PeerReply, FrameKind> {
        let request_id = match &frame {
            FrameKind::MetadataValue { request_id, .. } => *request_id,
            FrameKind::Error {
                request_id: Some(request_id),
                ..
            } => *request_id,
            _ => return Err(frame),
        };
        let Some(read) = self.pending.remove(&request_id) else {
            return Err(frame);
        };
        Ok(match frame {
            FrameKind::MetadataValue { value, .. } => PeerReply::Value(read, value),
            _ => PeerReply::Refused,
        })
    }

    /// A handover carries observed identity, never the previous entry's GET.
    pub(in crate::attach::driver) fn adopt_serving_host(&mut self, host: String) {
        self.serving_host = Some(host);
        self.serving_host_attempted = true;
    }
}
