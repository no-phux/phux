//! The Swift-facing value of a watched `Global` metadata key.

/// One value of a watched `Global` metadata key; `value` is `None` when the
/// key is absent or was deleted.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireGlobalMetadata {
    pub key: String,
    pub value: Option<Vec<u8>>,
}

mirror_from!(phux_client_runtime::control::GlobalMetadata => WireGlobalMetadata { key, value });
