//! TLV field ids inside message bodies (`docs/protocol.md#framing`).
//!
//! Each top-level field is `field_id: varint || wire_type: u8 || value`;
//! decoders skip unknown ids by length. Ids are per message, start at `1`,
//! and are stable within a major version: new fields append, removed ids are
//! retired, never reused. An optional field is simply absent. Nested unions
//! and sub-records stay positional inside a field's value.
//!
//! Each frame body's module is declared through `tlv_body!`, which also
//! generates the codec of a regular frame (`wire/tlv.rs`).

mod connection;
mod lifecycle;
mod metadata;
mod organization;

pub use connection::*;
pub use lifecycle::*;
pub use metadata::*;
pub use organization::*;

/// Every frame body's field module by name, as the frame catalog names it
/// (`wire/frame/catalog.rs`). Field modules of records nested inside a
/// positional value are not frame bodies and are not listed.
#[cfg(test)]
pub(crate) const FRAME_MODULES: &[(&str, &[(&str, u32)])] = &[
    ("hello", hello::FIELDS),
    ("hello_ok", hello_ok::FIELDS),
    ("ping", ping::FIELDS),
    ("terminal_output", terminal_output::FIELDS),
    ("attach", attach::FIELDS),
    ("input_key", input_key::FIELDS),
    ("input_mouse", input_mouse::FIELDS),
    ("input_focus", input_focus::FIELDS),
    ("input_paste", input_paste::FIELDS),
    ("input_terminal_reply", input_terminal_reply::FIELDS),
    ("frame_ack", frame_ack::FIELDS),
    ("viewport_resize", viewport_resize::FIELDS),
    ("attached", attached::FIELDS),
    ("attach_ready", attach_ready::FIELDS),
    ("detached", detached::FIELDS),
    ("bootstrap_begin", bootstrap_begin::FIELDS),
    ("bootstrap_chunk", bootstrap_chunk::FIELDS),
    ("bootstrap_ready", bootstrap_ready::FIELDS),
    ("history_request", history_request::FIELDS),
    ("history_page", history_page::FIELDS),
    ("bootstrap_tombstone", bootstrap_tombstone::FIELDS),
    ("history_tombstone", history_tombstone::FIELDS),
    ("history_rejected", history_rejected::FIELDS),
    ("bell", bell::FIELDS),
    ("error", error::FIELDS),
    ("get_metadata", get_metadata::FIELDS),
    ("set_metadata", set_metadata::FIELDS),
    ("list_metadata", list_metadata::FIELDS),
    ("subscribe_metadata", subscribe_metadata::FIELDS),
    ("metadata_changed", metadata_changed::FIELDS),
    ("metadata_value", metadata_value::FIELDS),
    ("metadata_keys", metadata_keys::FIELDS),
    ("list_directory", list_directory::FIELDS),
    ("directory_listing", directory_listing::FIELDS),
    ("path_query", path_query::FIELDS),
    ("path_results", path_results::FIELDS),
    ("spawn_terminal", spawn_terminal::FIELDS),
    ("terminal_spawned", terminal_spawned::FIELDS),
    ("move_terminal", move_terminal::FIELDS),
    ("terminal_moved", terminal_moved::FIELDS),
    ("terminal_closed", terminal_closed::FIELDS),
    ("terminal_resize", terminal_resize::FIELDS),
    ("command", command::FIELDS),
    ("command_result", command_result::FIELDS),
    ("subscribe_events", subscribe_events::FIELDS),
    ("event", event::FIELDS),
    ("clipboard_reply", clipboard_reply::FIELDS),
    ("frame_compressed", frame_compressed::FIELDS),
];
