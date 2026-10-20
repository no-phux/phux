//! The Swift-facing value of a watched `Global` metadata key.

/// One value of a watched `Global` metadata key; `value` is `None` when the
/// key is absent or was deleted.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireGlobalMetadata {
    pub key: String,
    pub value: Option<Vec<u8>>,
}

impl From<phux_client_runtime::control::GlobalMetadata> for WireGlobalMetadata {
    fn from(value: phux_client_runtime::control::GlobalMetadata) -> Self {
        Self {
            key: value.key,
            value: value.value,
        }
    }
}
