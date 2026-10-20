//! The link's HELLO exchange with a satellite, and validating its `HELLO_OK`.

use super::{
    BootstrapLimits, BootstrapProfile, BootstrapProfileKind, ClientCapabilities, FrameKind,
    LINK_SEND_TIMEOUT, LinkConn, LinkReader, LinkWriter, NegotiatedBootstrap, PROTOCOL_VERSION,
    send_bounded,
};

/// Complete the mandatory network-transport version handshake before the
/// relay session can send commands.
pub(super) async fn negotiate_link<C: LinkConn + LinkReader + LinkWriter>(
    conn: &mut C,
) -> Result<NegotiatedBootstrap, String> {
    let offered = hub_link_capabilities();
    let mut encoded = bytes::BytesMut::new();
    FrameKind::Hello {
        client_name: "phux-hub".to_owned(),
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        client_caps: offered,
    }
    .encode(&mut encoded);
    send_bounded(conn, &encoded).await?;

    let response = tokio::time::timeout(LINK_SEND_TIMEOUT, conn.recv_frame())
        .await
        .map_err(|_elapsed| {
            format!(
                "satellite did not answer HELLO within {}s",
                LINK_SEND_TIMEOUT.as_secs()
            )
        })??
        .ok_or_else(|| "satellite closed before HELLO_OK".to_owned())?;
    let (frame, rest) = FrameKind::decode(&response)
        .map_err(|error| format!("decode satellite HELLO response: {error:?}"))?;
    if !rest.is_empty() {
        return Err("satellite HELLO response contained trailing bytes".to_owned());
    }
    match frame {
        FrameKind::HelloOk {
            protocol_major,
            protocol_minor,
            protocol_patch,
            server_caps,
            selected_profile,
            bootstrap_limits,
            server_id,
            ..
        } => {
            validate_link_hello_ok(
                &offered,
                protocol_major,
                protocol_minor,
                protocol_patch,
                selected_profile,
                bootstrap_limits,
            )?;
            Ok(NegotiatedBootstrap {
                profile: selected_profile,
                limits: bootstrap_limits,
                server_features: server_caps.features,
                server_features_ext: server_caps.features_ext,
                server_id: <[u8; 16]>::try_from(server_id.as_slice()).ok(),
            })
        }
        FrameKind::Error { code, message, .. } => {
            Err(format!("satellite rejected HELLO: {code:?}: {message}"))
        }
        other => Err(format!("expected HELLO_OK from satellite, got {other:?}")),
    }
}

/// What the hub offers a satellite: defaults plus L3 (for the relayed
/// `LIST_DIRECTORY`) and the two mirrored agent-metadata keys (ADR-0136).
pub(super) fn hub_link_capabilities() -> ClientCapabilities {
    ClientCapabilities::default().with_layers(phux_protocol::caps::LayerSet::with(&[
        phux_protocol::caps::Layer::L3,
    ]))
}

pub(super) fn validate_link_hello_ok(
    offered: &ClientCapabilities,
    protocol_major: u16,
    protocol_minor: u16,
    protocol_patch: u16,
    selected_profile: BootstrapProfile,
    selected_limits: BootstrapLimits,
) -> Result<(), String> {
    if (protocol_major, protocol_minor, protocol_patch)
        != (
            PROTOCOL_VERSION.major,
            PROTOCOL_VERSION.minor,
            PROTOCOL_VERSION.patch,
        )
    {
        return Err(format!(
            "satellite HELLO_OK selected unsupported protocol {protocol_major}.{protocol_minor}.{protocol_patch}",
        ));
    }
    let profile_is_offered = match selected_profile {
        BootstrapProfile::NativeState { codec, features } => {
            offered
                .bootstrap
                .profiles
                .contains(BootstrapProfileKind::NativeState)
                && offered.bootstrap.native_codecs.contains(codec)
                && features.supports_native()
                && offered.bootstrap.native_features.intersect(features) == features
        }
        BootstrapProfile::SynthesizedVtRaw => offered
            .bootstrap
            .profiles
            .contains(BootstrapProfileKind::SynthesizedVtRaw),
        BootstrapProfile::SynthesizedVtStateSync => offered
            .bootstrap
            .profiles
            .contains(BootstrapProfileKind::SynthesizedVtStateSync),
        _ => false,
    };
    if !profile_is_offered {
        return Err(format!(
            "satellite HELLO_OK selected bootstrap profile outside the hub offer: {selected_profile:?}",
        ));
    }
    if offered.bootstrap.limits.intersect(selected_limits) != selected_limits {
        return Err(format!(
            "satellite HELLO_OK selected bootstrap limits outside the hub offer: chunk={} history_page={}",
            selected_limits.max_chunk_bytes(),
            selected_limits.max_history_page_bytes(),
        ));
    }
    Ok(())
}
