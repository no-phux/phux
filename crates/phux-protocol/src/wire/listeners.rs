//! Remote-listener bind outcomes on `GET_STATE` (phux-kyna).
//!
//! Carried as a trailing additive JSON object on [`super::info::SessionSnapshot`]:
//! when absent the snapshot is byte-identical to one encoded before this field
//! existed; an older decoder that stops after the session facets ignores it.

use serde::{Deserialize, Serialize};

/// Bumped when the JSON shape of [`RemoteListenersReport`] changes incompatibly.
pub const REMOTE_LISTENERS_SCHEMA_VERSION: u32 = 1;

/// Why a remote listener that was expected did not bind.
///
/// `snake_case` on the wire so `phux doctor --json` and log greps stay stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ListenerDisabledReason {
    /// `ReloadingTokenStore::load` failed; every secure remote transport stays down.
    TokenStoreLoadFailed,
    /// TLS acceptor / QUIC endpoint build failed after the cert was on disk.
    TlsSetupFailed,
    /// Self-signed (or operator) certificate could not be provisioned.
    CertProvisionFailed,
    /// The socket bind itself failed (address in use, permission, etc.).
    BindFailed,
    /// Auto-overlay gate closed (`PHUX_NO_AUTO_LISTEN`, non-default profile).
    OverlayGateClosed,
    /// Auto-overlay gate was open but no overlay address was detected.
    NoOverlayAddress,
}

impl ListenerDisabledReason {
    /// Stable `snake_case` label for doctor hints and logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TokenStoreLoadFailed => "token_store_load_failed",
            Self::TlsSetupFailed => "tls_setup_failed",
            Self::CertProvisionFailed => "cert_provision_failed",
            Self::BindFailed => "bind_failed",
            Self::OverlayGateClosed => "overlay_gate_closed",
            Self::NoOverlayAddress => "no_overlay_address",
        }
    }
}

impl std::fmt::Display for ListenerDisabledReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which remote transport a [`RemoteListenerSlot`] describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RemoteListenerTransport {
    /// Secure WebSocket (`wss://`), including the auto-bound overlay port.
    Wss,
    /// QUIC (always TLS).
    Quic,
    /// WebTransport (HTTP/3 over QUIC).
    Wt,
}

impl RemoteListenerTransport {
    /// Stable `snake_case` label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Wss => "wss",
            Self::Quic => "quic",
            Self::Wt => "wt",
        }
    }
}

impl std::fmt::Display for RemoteListenerTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One configured or auto-bound remote listener's bind outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RemoteListenerSlot {
    /// Transport this row describes.
    pub transport: RemoteListenerTransport,
    /// Whether this process intended to bind this transport.
    pub expected: bool,
    /// Whether a listener is currently accepting.
    pub bound: bool,
    /// Bound or intended socket address, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addr: Option<String>,
    /// Why `expected && !bound`, when the server knows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<ListenerDisabledReason>,
}

impl RemoteListenerSlot {
    /// A transport that was asked for and is listening.
    #[must_use]
    pub fn bound(transport: RemoteListenerTransport, addr: impl Into<String>) -> Self {
        Self {
            transport,
            expected: true,
            bound: true,
            addr: Some(addr.into()),
            disabled_reason: None,
        }
    }

    /// A transport that was asked for and did not bind.
    #[must_use]
    pub const fn disabled(
        transport: RemoteListenerTransport,
        addr: Option<String>,
        reason: ListenerDisabledReason,
    ) -> Self {
        Self {
            transport,
            expected: true,
            bound: false,
            addr,
            disabled_reason: Some(reason),
        }
    }

    /// True when doctor should treat this row as a problem.
    #[must_use]
    pub const fn is_unhealthy(&self) -> bool {
        self.expected && !self.bound
    }
}

/// Server-reported remote listener table on `GET_STATE` (schema v1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[non_exhaustive]
pub struct RemoteListenersReport {
    /// JSON schema version; unknown newer versions are still readable for known fields.
    pub schema_version: u32,
    /// One row per configured or auto-bound remote transport.
    pub listeners: Vec<RemoteListenerSlot>,
}

impl RemoteListenersReport {
    /// Empty report at the current schema version.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            schema_version: REMOTE_LISTENERS_SCHEMA_VERSION,
            listeners: Vec::new(),
        }
    }

    /// Builder: replace the listener rows.
    #[must_use]
    pub fn with_listeners(mut self, listeners: Vec<RemoteListenerSlot>) -> Self {
        self.listeners = listeners;
        self
    }

    /// Upsert a slot by transport (later writes win for the same transport).
    pub fn upsert(&mut self, slot: RemoteListenerSlot) {
        if let Some(existing) = self
            .listeners
            .iter_mut()
            .find(|row| row.transport == slot.transport)
        {
            *existing = slot;
        } else {
            self.listeners.push(slot);
        }
    }

    /// Rows where the server expected a bind and does not have one.
    pub fn unhealthy(&self) -> impl Iterator<Item = &RemoteListenerSlot> {
        self.listeners.iter().filter(|slot| slot.is_unhealthy())
    }

    /// Serialize for the trailing `SessionSnapshot` JSON field.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            format!("{{\"schema_version\":{REMOTE_LISTENERS_SCHEMA_VERSION},\"listeners\":[]}}")
        })
    }

    /// Parse a trailing JSON field; unknown fields are ignored by serde.
    #[must_use]
    pub fn from_json(raw: &str) -> Option<Self> {
        serde_json::from_str(raw).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_round_trip_preserves_disabled_reason() {
        let report =
            RemoteListenersReport::new().with_listeners(vec![RemoteListenerSlot::disabled(
                RemoteListenerTransport::Wss,
                Some("100.64.0.2:8787".into()),
                ListenerDisabledReason::TokenStoreLoadFailed,
            )]);
        let back = RemoteListenersReport::from_json(&report.to_json()).expect("parse");
        assert_eq!(back, report);
        assert_eq!(
            back.listeners[0].disabled_reason,
            Some(ListenerDisabledReason::TokenStoreLoadFailed)
        );
    }

    #[test]
    fn snapshot_stays_under_command_result_budget() {
        use crate::ids::ResourceId;
        use crate::wire::frame::{CommandResult, CommandValue};
        use crate::wire::info::SessionSnapshot;
        assert!(
            std::mem::size_of::<SessionSnapshot>() < 128,
            "SessionSnapshot={} must stay under 128 so CommandResult Err stays small",
            std::mem::size_of::<SessionSnapshot>()
        );
        assert!(std::mem::size_of::<CommandResult>() < 128);
        assert!(std::mem::size_of::<CommandValue>() < 128);
        let _ = std::mem::size_of::<ResourceId>();
    }
}
