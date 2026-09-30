//! Typed text as key events. The mapping lives in [`phux_client_core::keys`]
//! so the browser client, which does not link this runtime, shares it.

pub use phux_client_core::keys::{
    key_event_for_char, key_events_for_text, named, physical_key_for_char, press,
};
