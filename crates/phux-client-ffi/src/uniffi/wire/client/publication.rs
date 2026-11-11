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
            status: status::connection(Some(observation.status)).into(),
            last_error: observation.last_error,
            topology: observation.topology.map(|graph| {
                let replica = client.map(Client::organization);
                topology::session_graph(graph, replica.as_ref()).into()
            }),
        }
    }
}
