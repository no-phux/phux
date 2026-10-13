//! Opaque, type-distinct [`slotmap`] handles.
//!
//! Each is meaningful only relative to the
//! [`Registry`](crate::Registry) that issued it. Handles name live
//! records in memory and die with the server instance; persistent records
//! are named by [`crate::keys`] instead.

use slotmap::new_key_type;

new_key_type! {
    /// Live handle of a [`Session`](crate::Session) record.
    pub struct SessionId;

    /// Live handle of a [`Resource`](crate::Resource) record. Every kind
    /// shares this handle space.
    pub struct ResourceId;
}
