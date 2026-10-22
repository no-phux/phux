//! Payload writers for the L3 metadata frames.

use crate::wire::encode::Encoder;
use crate::wire::field;
use crate::wire::frame::{Scope, encode_scope};

use super::{FrameKind, u32_field};

impl FrameKind {
    /// Write the `{request_id, scope, key}` body shared by GET and DELETE.
    pub(super) fn encode_get_or_delete_metadata(
        enc: &mut Encoder<'_>,
        request_id: u32,
        scope: &Scope,
        key: &str,
    ) {
        u32_field(enc, field::get_metadata::REQUEST_ID, request_id);
        enc.write_field_with(field::get_metadata::SCOPE, |e| encode_scope(scope, e));
        enc.write_field(field::get_metadata::KEY, key.as_bytes());
    }

    /// Write the `SET_METADATA` payload.
    pub(super) fn encode_set_metadata(
        enc: &mut Encoder<'_>,
        request_id: u32,
        scope: &Scope,
        key: &str,
        value: &[u8],
    ) {
        u32_field(enc, field::set_metadata::REQUEST_ID, request_id);
        enc.write_field_with(field::set_metadata::SCOPE, |e| encode_scope(scope, e));
        enc.write_field(field::set_metadata::KEY, key.as_bytes());
        enc.write_field(field::set_metadata::VALUE, value);
    }

    /// Write the `LIST_METADATA` payload.
    pub(super) fn encode_list_metadata(enc: &mut Encoder<'_>, request_id: u32, scope: &Scope) {
        u32_field(enc, field::list_metadata::REQUEST_ID, request_id);
        enc.write_field_with(field::list_metadata::SCOPE, |e| encode_scope(scope, e));
    }

    /// Write the `SUBSCRIBE_METADATA` payload.
    pub(super) fn encode_subscribe_metadata(enc: &mut Encoder<'_>, scope: &Scope, key: &str) {
        enc.write_field_with(field::subscribe_metadata::SCOPE, |e| encode_scope(scope, e));
        enc.write_field(field::subscribe_metadata::KEY, key.as_bytes());
    }

    /// Write the `METADATA_CHANGED` payload.
    pub(super) fn encode_metadata_changed(
        enc: &mut Encoder<'_>,
        scope: &Scope,
        key: &str,
        value: Option<&[u8]>,
    ) {
        enc.write_field_with(field::metadata_changed::SCOPE, |e| encode_scope(scope, e));
        enc.write_field(field::metadata_changed::KEY, key.as_bytes());
        // Optional value: absent field = tombstone (None).
        if let Some(v) = value {
            enc.write_field(field::metadata_changed::VALUE, v);
        }
    }

    /// Write the `METADATA_VALUE` payload.
    pub(super) fn encode_metadata_value(
        enc: &mut Encoder<'_>,
        request_id: u32,
        value: Option<&[u8]>,
    ) {
        u32_field(enc, field::metadata_value::REQUEST_ID, request_id);
        // Optional value: absent field = key absent (None).
        if let Some(v) = value {
            enc.write_field(field::metadata_value::VALUE, v);
        }
    }

    /// Write the `METADATA_KEYS` payload.
    pub(super) fn encode_metadata_keys(enc: &mut Encoder<'_>, request_id: u32, keys: &[String]) {
        u32_field(enc, field::metadata_keys::REQUEST_ID, request_id);
        // The keys list is one field whose value is a positional u32
        // count + N length-prefixed strings (present even when empty).
        enc.write_field_with(field::metadata_keys::KEYS, |e| {
            debug_assert!(
                u32::try_from(keys.len()).is_ok(),
                "metadata keys list length exceeds u32",
            );
            let len = u32::try_from(keys.len()).unwrap_or(u32::MAX);
            e.write_u32_be(len);
            for k in keys {
                e.write_str(k);
            }
        });
    }
}
