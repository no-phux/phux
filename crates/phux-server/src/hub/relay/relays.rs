//! The registry of per-satellite relay handles, and failing requests while a link is down.

use super::{
    Arc, BTreeMap, CommandResult, ErrorCode, FrameKind, Mutex, Outbound, RelayHandle, RelayRequest,
    SatelliteHost, SpawnError, SpawnResult, listing_refusal, path_refusal, trace,
};

/// Shared registry of per-satellite [`RelayHandle`]s; empty on a non-hub
/// server.
#[derive(Debug, Clone, Default)]
pub(crate) struct HubRelays {
    inner: Arc<Mutex<BTreeMap<SatelliteHost, RelayHandle>>>,
}

impl HubRelays {
    /// Register `handle` for its host (hub bring-up, one per table entry).
    pub(crate) fn insert(&self, handle: RelayHandle) {
        self.lock().insert(handle.host.clone(), handle);
    }

    /// Drop `host`'s handle (a registry reload stopped its link).
    pub(crate) fn remove(&self, host: &SatelliteHost) {
        self.lock().remove(host);
    }

    /// The relay handle for `host`, if the hub dials that satellite.
    pub(crate) fn get(&self, host: &SatelliteHost) -> Option<RelayHandle> {
        self.lock().get(host).cloned()
    }

    /// Every registered handle (detach fan-out).
    pub(crate) fn all(&self) -> Vec<RelayHandle> {
        self.lock().values().cloned().collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<SatelliteHost, RelayHandle>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Fail one queued request while the link is not connected.
pub(crate) fn fail_fast(request: RelayRequest, host: &SatelliteHost, why: &str) {
    match request {
        // Nothing was registered; failing the oneshot is enough.
        RelayRequest::Command { reply, .. } | RelayRequest::Keyed { reply, .. } => {
            let _ = reply.send(CommandResult::Error {
                code: ErrorCode::SatelliteUnreachable,
                message: format!("satellite {host} is unreachable: {why}"),
            });
        }
        RelayRequest::Spawn { reply, .. } => {
            let _ = reply.send(SpawnResult::Err(SpawnError::SatelliteUnreachable(format!(
                "satellite {host} is unreachable: {why}"
            ))));
        }
        RelayRequest::Forward { frame } => {
            trace!(satellite = %host, kind = ?frame_label(&frame), why, "relay frame dropped while disconnected");
        }
        RelayRequest::ListDirectory { path, reply } => {
            let _ = reply.send(Err(listing_refusal(
                &path,
                format!("satellite {host} is unreachable: {why}"),
            )));
        }
        RelayRequest::PathQuery { root, reply, .. } => {
            let _ = reply.send(Err(path_refusal(
                &root,
                format!("satellite {host} is unreachable: {why}"),
            )));
        }
        RelayRequest::Subscribe { subscription, .. } => {
            // Notify like a disconnect would; nothing to roll back.
            let _ = subscription
                .out_tx
                .try_send(Outbound::Frame(unreachable_error(host, why)));
            trace!(
                satellite = %host,
                client = ?subscription.client,
                why,
                "proxy subscription refused while disconnected"
            );
        }
        RelayRequest::MirrorTerminal { terminal } => {
            // Not marked mirrored; the next inventory retries.
            trace!(
                satellite = %host,
                terminal,
                why,
                "agent metadata mirror dropped while disconnected"
            );
        }
    }
}

/// The typed `ERROR` for an unreachable satellite.
pub(super) fn unreachable_error(host: &SatelliteHost, why: &str) -> FrameKind {
    FrameKind::Error {
        request_id: None,
        code: ErrorCode::SatelliteUnreachable,
        message: format!("satellite {host} is unreachable: {why}"),
    }
}

/// A payload-free label for logging a relayed frame.
pub(super) const fn frame_label(frame: &FrameKind) -> &'static str {
    match frame {
        FrameKind::InputKey { .. } => "INPUT_KEY",
        FrameKind::InputMouse { .. } => "INPUT_MOUSE",
        FrameKind::InputFocus { .. } => "INPUT_FOCUS",
        FrameKind::InputPaste { .. } => "INPUT_PASTE",
        FrameKind::FrameAck { .. } => "FRAME_ACK",
        FrameKind::ResizeTerminal { .. } => "RESIZE_TERMINAL",
        FrameKind::SubscribeEvents { .. } => "SUBSCRIBE_EVENTS",
        _ => "other",
    }
}
