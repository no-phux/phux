//! Connection status and negotiated capabilities, as a product reads them.

use phux_client_runtime::control::{ServerInfo, Status};
use phux_protocol::caps::{ServerFeature, ServerFeatureExt};

/// Lossless, opaque server-incarnation identity. This is encoding, not a hash
/// or a UTF-8 interpretation; even empty/non-UTF-8 identities retain every byte.
#[must_use]
pub fn server_id(server: &ServerInfo) -> String {
    use std::fmt::Write as _;
    server.id.iter().fold(
        String::with_capacity(server.id.len() * 2),
        |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        },
    )
}

/// What a consumer shows for the connection.
///
/// The runtime distinguishes `Idle`, `Connecting` and `Negotiated`; a product
/// shows one spinner for all three, because until the attach barrier releases
/// there is nothing to paint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connection {
    /// Dialing, negotiating, or waiting out the reconnect ladder.
    Connecting,
    /// The attach barrier released; frames are flowing.
    Attached,
    /// The consumer ended the session. Terminal.
    Closed,
    /// A refusal no retry can satisfy. Terminal.
    Failed,
}

/// Fold a runtime status, or its absence before a connection exists.
#[must_use]
pub const fn connection(status: Option<Status>) -> Connection {
    match status {
        None | Some(Status::Idle | Status::Connecting | Status::Negotiated) => {
            Connection::Connecting
        }
        Some(Status::Attached) => Connection::Attached,
        Some(Status::Closed) => Connection::Closed,
        Some(Status::Failed) => Connection::Failed,
    }
}

/// The optional server features this session negotiated, named as the
/// products name them.
#[must_use]
pub fn negotiated_features(server: &ServerInfo) -> Vec<String> {
    [
        (ServerFeature::AcknowledgedInput, "acknowledged-input"),
        (ServerFeature::FileUpload, "file-upload"),
        (ServerFeature::Transcribe, "transcribe"),
        (ServerFeature::ListDirectory, "list-directory"),
    ]
    .into_iter()
    .filter(|(feature, _)| server.features.contains(*feature))
    .map(|(_, name)| name.to_owned())
    .chain(
        [
            (ServerFeatureExt::PathQuery, "path-query"),
            (ServerFeatureExt::Checkout, "checkout"),
        ]
        .into_iter()
        .filter(|(feature, _)| server.features_ext.contains(*feature))
        .map(|(_, name)| name.to_owned()),
    )
    .collect()
}

/// The protocol version `server` speaks, as `major.minor.patch`.
#[must_use]
pub fn protocol_version(server: &ServerInfo) -> String {
    format!(
        "{}.{}.{}",
        server.protocol.0, server.protocol.1, server.protocol.2
    )
}

#[cfg(test)]
mod tests {
    use super::{Connection, connection};
    use phux_client_runtime::control::Status;

    #[test]
    fn everything_before_the_attach_barrier_is_connecting() {
        for status in [Status::Idle, Status::Connecting, Status::Negotiated] {
            assert_eq!(connection(Some(status)), Connection::Connecting);
        }
        assert_eq!(connection(None), Connection::Connecting);
    }

    #[test]
    fn checkout_is_named_only_when_advertised() {
        use phux_protocol::caps::{
            BootstrapLimits, LayerSet, ServerFeatureExt, ServerFeatureExtSet, ServerFeatureSet,
        };
        let mut server = super::ServerInfo {
            id: Vec::new(),
            features: ServerFeatureSet::new(),
            features_ext: ServerFeatureExtSet::new(),
            layers: LayerSet::all(),
            protocol: (0, 9, 0),
            profile: phux_protocol::BootstrapProfile::SynthesizedVtRaw,
            limits: BootstrapLimits::default(),
        };
        assert!(super::negotiated_features(&server).is_empty());
        server.features_ext = ServerFeatureExtSet::with(&[ServerFeatureExt::Checkout]);
        assert_eq!(super::negotiated_features(&server), ["checkout"]);
    }

    #[test]
    fn terminal_states_are_distinguished() {
        assert_eq!(connection(Some(Status::Attached)), Connection::Attached);
        assert_eq!(connection(Some(Status::Closed)), Connection::Closed);
        assert_eq!(connection(Some(Status::Failed)), Connection::Failed);
    }
}
