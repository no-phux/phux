//! L3 metadata and host-query frame bodies.

use crate::wire::frame::{ActorRef, Scope};

tlv_body! {
    /// `GET_METADATA` / `DELETE_METADATA` body fields (`docs/protocol.md#commands-events-resources`).
    pub mod get_metadata
    for GetMetadata => decode_get_metadata, DeleteMetadata => decode_delete_metadata {
        /// Correlating `request_id` (`u32`).
        pub const REQUEST_ID: u32 = 1 => default request_id: u32;
        /// `Scope` tagged union (positional).
        pub const SCOPE: u32 = 2 => required scope: Scope;
        /// Metadata key string.
        pub const KEY: u32 = 3 => required key: String;
    }
}

tlv_body! {
    /// `SET_METADATA` body fields (`docs/protocol.md#commands-events-resources`).
    pub mod set_metadata for SetMetadata => decode_set_metadata {
        /// Correlating `request_id` (`u32`).
        pub const REQUEST_ID: u32 = 1 => default request_id: u32;
        /// `Scope` tagged union (positional).
        pub const SCOPE: u32 = 2 => required scope: Scope;
        /// Metadata key string.
        pub const KEY: u32 = 3 => required key: String;
        /// Metadata value bytes.
        pub const VALUE: u32 = 4 => default value: Vec<u8>;
    }
}

tlv_body! {
    /// `LIST_METADATA` body fields (`docs/protocol.md#commands-events-resources`).
    pub mod list_metadata for ListMetadata => decode_list_metadata {
        /// Correlating `request_id` (`u32`).
        pub const REQUEST_ID: u32 = 1 => default request_id: u32;
        /// `Scope` tagged union (positional).
        pub const SCOPE: u32 = 2 => required scope: Scope;
    }
}

tlv_body! {
    /// `SUBSCRIBE_METADATA` body fields (`docs/protocol.md#commands-events-resources`).
    pub mod subscribe_metadata for SubscribeMetadata => decode_subscribe_metadata {
        /// `Scope` tagged union (positional).
        pub const SCOPE: u32 = 1 => required scope: Scope;
        /// Metadata key string.
        pub const KEY: u32 = 2 => required key: String;
    }
}

tlv_body! {
    /// `METADATA_CHANGED` body fields (`docs/protocol.md#commands-events-resources`).
    pub mod metadata_changed for MetadataChanged => decode_metadata_changed {
        /// `Scope` tagged union (positional).
        pub const SCOPE: u32 = 1 => required scope: Scope;
        /// Metadata key string.
        pub const KEY: u32 = 2 => required key: String;
        /// Optional new value bytes (absent field = `None` / tombstone).
        pub const VALUE: u32 = 3 => optional value: Vec<u8>;
        /// Optional `ActorRef` of the connection whose write caused the change
        /// (positional; ADR-0123, gated on `ServerFeature::EventJournal`).
        pub const ACTOR: u32 = 4 => optional actor: ActorRef;
    }
}

tlv_body! {
    /// `METADATA_VALUE` body fields (`docs/protocol.md#commands-events-resources`).
    pub mod metadata_value for MetadataValue => decode_metadata_value {
        /// Correlating `request_id` (`u32`).
        pub const REQUEST_ID: u32 = 1 => default request_id: u32;
        /// Optional value bytes (absent field = key absent).
        pub const VALUE: u32 = 2 => optional value: Vec<u8>;
    }
}

tlv_body! {
    /// `METADATA_KEYS` body fields (`docs/protocol.md#commands-events-resources`).
    pub mod metadata_keys for MetadataKeys => decode_metadata_keys {
        /// Correlating `request_id` (`u32`).
        pub const REQUEST_ID: u32 = 1 => default request_id: u32;
        /// Sorted list of key names (positional `u32` count + strings).
        pub const KEYS: u32 = 2 => default keys: Vec<String>;
    }
}

tlv_body! {
    /// `LIST_DIRECTORY` body fields (`docs/protocol.md#commands-events-resources`).
    pub mod list_directory {
        /// Correlating `request_id` (`u32`).
        pub const REQUEST_ID: u32 = 1;
        /// Requested path (UTF-8). Empty or `~` = the serving user's home.
        pub const PATH: u32 = 2;
        /// Optional satellite host to list on via the hub (`LIST_DIRECTORY_HOST`).
        pub const HOST: u32 = 3;
    }
}

tlv_body! {
    /// `DIRECTORY_LISTING` body fields (`docs/protocol.md#commands-events-resources`).
    pub mod directory_listing {
        /// Correlating `request_id` (`u32`).
        pub const REQUEST_ID: u32 = 1;
        /// Resolved absolute path (or the attempted path on a refusal).
        pub const PATH: u32 = 2;
        /// Optional lexical parent (absent at the root or on a refusal).
        pub const PARENT: u32 = 3;
        /// Child directories: positional `u32` count + (name str, flags `u8`).
        pub const ENTRIES: u32 = 4;
        /// Optional truncation flag (`u8`, absent = not truncated).
        pub const TRUNCATED: u32 = 5;
        /// Optional `DirectoryErrorCode` (`u8`); present = the listing was refused.
        pub const ERROR: u32 = 6;
        /// Optional diagnostic text accompanying `ERROR`.
        pub const MESSAGE: u32 = 7;
    }
}

tlv_body! {
    /// `PATH_QUERY` body fields (`docs/protocol.md#commands-events-resources`).
    pub mod path_query {
        /// Correlation id (`u32`).
        pub const REQUEST_ID: u32 = 1;
        /// Starting directory (`str`).
        pub const ROOT: u32 = 2;
        /// Browse filter or fuzzy term (`str`).
        pub const QUERY: u32 = 3;
        /// `u8`: 0 = one level, 1 = recursive search.
        pub const RECURSIVE: u32 = 4;
        /// Optional satellite host (`str`).
        pub const HOST: u32 = 5;
    }
}

tlv_body! {
    /// `PATH_RESULTS` body fields (`docs/protocol.md#commands-events-resources`).
    pub mod path_results {
        /// Correlation id (`u32`).
        pub const REQUEST_ID: u32 = 1;
        /// Absolute lexical root (`str`), or attempted root on refusal.
        pub const ROOT: u32 = 2;
        /// Lexical parent (`str`), absent at filesystem root.
        pub const PARENT: u32 = 3;
        /// Positional `u32` count then `(path: str, kind: u8)` rows.
        pub const ROWS: u32 = 4;
        /// `PathStatus` (`u8`), required on success.
        pub const STATUS: u32 = 5;
        /// `PathErrorCode` (`u8`), present on refusal only.
        pub const ERROR: u32 = 6;
        /// Human-readable diagnostic, present on refusal only.
        pub const MESSAGE: u32 = 7;
    }
}
