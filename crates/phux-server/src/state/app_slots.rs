//! State an app keeps in [`ServerState`] without the host knowing its type.
//!
//! Each app owns its slot types and reaches them through
//! [`ServerState::app_slot`] and [`ServerState::app_slot_mut`]; the host
//! holds them by [`TypeId`] and only tells each one when a resource is
//! reaped.

use std::any::{Any, TypeId};
use std::collections::HashMap;

use phux_core::ids::ResourceId;
use phux_protocol::ids::ResourceId as WireResourceId;

use super::ServerState;

/// One app's state in [`ServerState`].
pub(crate) trait AppSlot: Any + Send + std::fmt::Debug {
    /// Drop what this slot keys on a reaped resource; `wire` is its retired
    /// wire id, when it had one.
    fn forget_resource(&mut self, resource: ResourceId, wire: Option<&WireResourceId>);
}

/// Every app's slot, by type.
#[derive(Debug, Default)]
pub(super) struct AppSlots(HashMap<TypeId, Box<dyn AppSlot>>);

impl AppSlots {
    /// Tell every slot a resource was reaped.
    pub(super) fn forget_resource(&mut self, resource: ResourceId, wire: Option<&WireResourceId>) {
        for slot in self.0.values_mut() {
            slot.forget_resource(resource, wire);
        }
    }
}

impl ServerState {
    /// The app slot of type `T`; `None` until it is first written.
    #[must_use]
    pub(crate) fn app_slot<T: AppSlot>(&self) -> Option<&T> {
        let slot: &dyn Any = self.app_slots.0.get(&TypeId::of::<T>())?.as_ref();
        slot.downcast_ref()
    }

    /// The app slot of type `T`, created empty on first use.
    #[allow(
        clippy::expect_used,
        reason = "the map is keyed by the type it stores, so the downcast cannot miss"
    )]
    pub(crate) fn app_slot_mut<T: AppSlot + Default>(&mut self) -> &mut T {
        let slot: &mut dyn Any = self
            .app_slots
            .0
            .entry(TypeId::of::<T>())
            .or_insert_with(|| Box::<T>::default())
            .as_mut();
        slot.downcast_mut().expect("slot keyed by its own type")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default)]
    struct Counts(Vec<ResourceId>);

    impl AppSlot for Counts {
        fn forget_resource(&mut self, resource: ResourceId, _wire: Option<&WireResourceId>) {
            self.0.retain(|kept| *kept != resource);
        }
    }

    #[test]
    fn a_slot_is_empty_until_written_and_forgets_reaped_resources() {
        let mut state = ServerState::new();
        assert!(state.app_slot::<Counts>().is_none());
        let resource = ResourceId::default();
        state.app_slot_mut::<Counts>().0.push(resource);
        assert_eq!(state.app_slot::<Counts>().map(|c| c.0.len()), Some(1));
        state.app_slots.forget_resource(resource, None);
        assert_eq!(state.app_slot::<Counts>().map(|c| c.0.len()), Some(0));
    }
}
