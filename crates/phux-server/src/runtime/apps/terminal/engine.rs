//! The injected Terminal engine, kept in the Terminal app's state slot.

use std::sync::Arc;

use phux_terminal_api::TerminalEngineFactory;

use crate::state::{AppSlot, SharedState};

/// The engine the executable composed in; none in host-only state.
#[derive(Debug, Default)]
struct EngineSlot(Option<Arc<dyn TerminalEngineFactory>>);

impl AppSlot for EngineSlot {
    fn forget_resource(
        &mut self,
        _resource: phux_core::ids::ResourceId,
        _wire: Option<&phux_protocol::ids::ResourceId>,
    ) {
    }
}

impl SharedState {
    /// Supply the Terminal engine for this state, before creating resources.
    pub fn set_terminal_engine(&self, engine: Arc<dyn TerminalEngineFactory>) {
        self.with_mut(|state| state.app_slot_mut::<EngineSlot>().0 = Some(engine));
    }

    /// The Terminal engine, once supplied.
    pub(crate) fn terminal_engine(&self) -> Option<Arc<dyn TerminalEngineFactory>> {
        self.with(|state| {
            state
                .app_slot::<EngineSlot>()
                .and_then(|slot| slot.0.clone())
        })
    }
}
