//! Activate Terminal publications after the host's aggregate barrier.
#![allow(clippy::wildcard_imports, reason = "shared attach namespace")]

use super::*;

#[cfg_attr(
    not(all(feature = "native-engine", not(target_arch = "wasm32"))),
    allow(
        unused_variables,
        unused_mut,
        reason = "native publication activation is optional"
    )
)]
pub(super) async fn release_gates(
    state: &SharedState,
    connection_token: &CancellationToken,
    client_id: ClientId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    gates: Vec<SnapshotGate>,
) {
    for gate in gates {
        let Some(cut) = gate.cut else {
            continue;
        };
        let mut start = OutputPumpStart {
            published_cut: cut,
            replay: Vec::new(),
            live: None,
        };
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        if let Some(cursor) = gate.native_cursor {
            let publication = match activate_native_publication(
                &gate.terminal,
                client_id.0,
                gate.wire_terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
            )
            .await
            {
                Ok(publication) => publication,
                // The resource closed after capture: no generation can be
                // published, but RESOURCE_CLOSED ends it without an error.
                Err(failure) if failure.actor_gone() => continue,
                Err(_) => {
                    crate::runtime::connections::detach_and_release_consumer_state(
                        state, client_id,
                    );
                    connection_token.cancel();
                    return;
                }
            };
            start.replay = publication.replay;
            start.live = Some(publication.live);
        }
        let _ = gate.gate.send(start);
    }
}
