//! Which resources this window may place: terminals that accept input and
//! surfaces that have a frame.
use super::*;

impl Connection {
    /// The terminal is inventoried or explicitly admitted, and accepts input.
    #[must_use]
    pub fn ready(&self, id: &ResourceId) -> bool {
        !self.closed
            && (self.known.contains(id) || self.admitted.contains(id))
            && (self.surfaces.contains_key(id) || self.client.input_ready(id))
    }

    /// The live pixel surfaces of session `name`, from the organization
    /// replica: the topology lists terminals only.
    #[must_use]
    pub fn surfaces_in(&self, name: &str) -> Vec<ResourceId> {
        self.with_organization(|replica| {
            let session = replica
                .sessions()
                .iter()
                .find(|session| session.name == name)
                .map(|session| session.key);
            replica
                .resources()
                .iter()
                .filter(|record| {
                    record.kind == ResourceKind::PixelSurface && Some(record.session) == session
                })
                .filter_map(|record| record.live.clone())
                .collect()
        })
    }
}
