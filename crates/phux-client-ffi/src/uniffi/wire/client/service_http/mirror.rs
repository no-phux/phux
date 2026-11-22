//! A host's screen mirrors (`MIRROR`, behind `features_ext.MIRRORS`): list
//! the displays and windows it can mirror, start one in its
//! `mirror-<target>` session (an existing one is returned), or stop one.
//! `host` names a satellite of the connected hub; `None` is the connected
//! host itself. `negotiated_features` names the capability `mirrors`.

use phux_client_runtime::control::MirrorError;
use phux_protocol::wire::frame::{ErrorCode, MirrorOp};
use phux_protocol::{ResourceId, ids::SatelliteHost};

use super::super::RemoteClient;
use crate::projection::id;

/// One display or window a host can mirror.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireCaptureTarget {
    /// What to pass to `start_mirror`.
    pub target: String,
    /// `display` or `window`.
    pub kind: String,
    /// What to show.
    pub label: String,
}

/// A started (or already running) mirror.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireMirrorStarted {
    /// The session it runs in.
    pub session: String,
    /// Its pixel surface, to open in the document viewer; `None` when it
    /// has not published a frame yet (it appears in the topology later).
    pub surface_id: Option<String>,
}

/// Why a mirror call has no answer.
#[derive(uniffi::Error, Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WireMirrorError {
    /// Nothing was sent: no connection is established.
    #[error("not connected")]
    NotConnected,
    /// Nothing was sent: the host's phux is too old to mirror screens.
    #[error("{reason}")]
    Unsupported { reason: String },
    /// The connection ended before the answer.
    #[error("the connection ended before the server answered")]
    Disconnected,
    /// The host cannot do it now (on a Mac, Rune is not open); `reason`
    /// says what to do.
    #[error("{reason}")]
    Unavailable { reason: String },
    /// The connection's grant does not allow observing that display or
    /// window.
    #[error("{reason}")]
    PermissionDenied { reason: String },
    /// Any other refusal.
    #[error("{reason}")]
    Refused { code: u16, reason: String },
}

impl From<MirrorError> for WireMirrorError {
    fn from(error: MirrorError) -> Self {
        let reason = error.to_string();
        match error {
            MirrorError::NotConnected => Self::NotConnected,
            MirrorError::Unsupported => Self::Unsupported { reason },
            MirrorError::Disconnected => Self::Disconnected,
            MirrorError::Refused { code, message } => match code {
                ErrorCode::Unavailable => Self::Unavailable { reason: message },
                ErrorCode::UnsupportedSatelliteRoute => Self::Unsupported { reason: message },
                ErrorCode::PermissionDenied => Self::PermissionDenied { reason: message },
                other => Self::Refused {
                    code: other.as_wire(),
                    reason: message,
                },
            },
        }
    }
}

/// The targets in a `MIRROR { Targets }` answer.
fn targets(answer: &serde_json::Value) -> Vec<WireCaptureTarget> {
    let text = |t: &serde_json::Value, key: &str| t[key].as_str().map(str::to_owned);
    answer["targets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| {
            Some(WireCaptureTarget {
                target: text(t, "target")?,
                kind: text(t, "kind")?,
                label: text(t, "label").unwrap_or_default(),
            })
        })
        .collect()
}

/// A `MIRROR { Start }` answer, its surface named on `host`.
fn started(host: Option<&str>, answer: &serde_json::Value) -> WireMirrorStarted {
    let surface = answer["surface"]
        .as_u64()
        .and_then(|raw| u32::try_from(raw).ok())
        .map(|raw| {
            host.map_or_else(
                || ResourceId::local(raw),
                |host| ResourceId::Satellite {
                    host: SatelliteHost::new(host),
                    id: raw,
                },
            )
        });
    WireMirrorStarted {
        session: answer["session"].as_str().unwrap_or_default().to_owned(),
        surface_id: surface.as_ref().map(id::encode),
    }
}

#[uniffi::export]
impl RemoteClient {
    /// The displays and windows `host` (or the connected host) can mirror.
    pub async fn capture_targets(
        &self,
        host: Option<String>,
    ) -> Result<Vec<WireCaptureTarget>, WireMirrorError> {
        let client = self.runtime_client().ok_or(WireMirrorError::NotConnected)?;
        Ok(targets(&client.mirror(host, MirrorOp::Targets).await?))
    }

    /// Start mirroring `target` (a `WireCaptureTarget::target`) on `host`,
    /// or find the mirror already running. Waits up to about 20 s for the
    /// first frame.
    pub async fn start_mirror(
        &self,
        host: Option<String>,
        target: String,
        jpeg: bool,
    ) -> Result<WireMirrorStarted, WireMirrorError> {
        let client = self.runtime_client().ok_or(WireMirrorError::NotConnected)?;
        let answer = client
            .mirror(host.clone(), MirrorOp::Start { target, jpeg })
            .await?;
        Ok(started(host.as_deref(), &answer))
    }

    /// Stop a mirror `start_mirror` began; a configured one is refused.
    pub async fn stop_mirror(
        &self,
        host: Option<String>,
        target: String,
    ) -> Result<(), WireMirrorError> {
        let client = self.runtime_client().ok_or(WireMirrorError::NotConnected)?;
        client.mirror(host, MirrorOp::Stop { target }).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_become_records() {
        let answer = serde_json::json!({ "targets": [
            { "target": "display", "kind": "display", "label": "Main display" },
            { "kind": "window" },
        ]});
        assert_eq!(
            targets(&answer),
            [WireCaptureTarget {
                target: "display".into(),
                kind: "display".into(),
                label: "Main display".into(),
            }]
        );
        let answer = serde_json::json!({ "session": "mirror-display", "surface": 7 });
        assert_eq!(
            started(Some("box"), &answer).surface_id.as_deref(),
            Some("satellite:box:7")
        );
        assert_eq!(
            started(None, &answer).surface_id.as_deref(),
            Some("local:7")
        );
        let pending = serde_json::json!({ "session": "mirror-display", "surface": null });
        assert_eq!(started(None, &pending).surface_id, None);
    }
}
