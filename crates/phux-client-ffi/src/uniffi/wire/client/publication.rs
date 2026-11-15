//! One drained observation lowered to the binding's publication.

use super::*;

impl RemoteClient {
    /// Lower `observation`, drained from `client`, keeping its connection
    /// boundaries and sampling the session graph against the same client.
    pub(super) fn publication(
        &self,
        client: Option<&Client>,
        observation: Observation,
    ) -> WirePublication {
        let mut events = Vec::new();
        for event in observation.events {
            match event {
                Event::ConnectionLost { message } => {
                    events.push(WireEvent::ConnectionLost { message });
                }
                Event::ConnectionOpened { connection_epoch } => {
                    events.push(WireEvent::ConnectionOpened { connection_epoch });
                }
                event => self.project_event(client, event, &mut events),
            }
        }
        WirePublication {
            connection_epoch: observation.connection_epoch,
            events,
            events_dropped: observation.events_dropped,
            status: WireStatus::from(Some(observation.status)),
            last_error: observation.last_error,
            topology: observation.topology.map(|graph| {
                let replica = client.map(Client::organization);
                session_topology(graph, replica.as_ref())
            }),
        }
    }
}

#[uniffi::export]
impl RemoteClient {
    /// The latest measured round trip to the server in microseconds, or
    /// `None` before a liveness probe is answered on this connection.
    /// `nudge` sends a probe, so a health view refreshes it by nudging.
    pub fn round_trip_micros(&self) -> Option<u32> {
        let round_trip = self.runtime_client()?.round_trip()?;
        Some(u32::try_from(round_trip.as_micros()).unwrap_or(u32::MAX))
    }
}
