//! Payload writers for the L3 metadata frames and the host path queries.

use crate::wire::encode::Encoder;
use crate::wire::field;
use crate::wire::frame::directory::{encode_directory_listing, encode_list_directory};
use crate::wire::frame::path::{encode_query, encode_results};
use crate::wire::frame::{encode_actor_ref, encode_scope};

use super::{FrameKind, u32_field};

impl FrameKind {
    /// Write a metadata or host-query frame's payload.
    pub(super) fn encode_metadata_payload(&self, enc: &mut Encoder<'_>) {
        match self {
            // GET and DELETE share one body; the type byte tells them apart.
            Self::GetMetadata {
                request_id,
                scope,
                key,
            }
            | Self::DeleteMetadata {
                request_id,
                scope,
                key,
            } => {
                u32_field(enc, field::get_metadata::REQUEST_ID, *request_id);
                enc.write_field_with(field::get_metadata::SCOPE, |e| encode_scope(scope, e));
                enc.write_field(field::get_metadata::KEY, key.as_bytes());
            }
            Self::SetMetadata {
                request_id,
                scope,
                key,
                value,
            } => {
                u32_field(enc, field::set_metadata::REQUEST_ID, *request_id);
                enc.write_field_with(field::set_metadata::SCOPE, |e| encode_scope(scope, e));
                enc.write_field(field::set_metadata::KEY, key.as_bytes());
                enc.write_field(field::set_metadata::VALUE, value);
            }
            Self::ListMetadata { request_id, scope } => {
                u32_field(enc, field::list_metadata::REQUEST_ID, *request_id);
                enc.write_field_with(field::list_metadata::SCOPE, |e| encode_scope(scope, e));
            }
            Self::SubscribeMetadata { scope, key } => {
                enc.write_field_with(field::subscribe_metadata::SCOPE, |e| encode_scope(scope, e));
                enc.write_field(field::subscribe_metadata::KEY, key.as_bytes());
            }
            Self::MetadataChanged {
                scope,
                key,
                value,
                actor,
            } => {
                enc.write_field_with(field::metadata_changed::SCOPE, |e| encode_scope(scope, e));
                enc.write_field(field::metadata_changed::KEY, key.as_bytes());
                // An absent value is a tombstone.
                if let Some(value) = value {
                    enc.write_field(field::metadata_changed::VALUE, value);
                }
                if let Some(actor) = actor {
                    enc.write_field_with(field::metadata_changed::ACTOR, |e| {
                        encode_actor_ref(actor, e);
                    });
                }
            }
            Self::MetadataValue { request_id, value } => {
                u32_field(enc, field::metadata_value::REQUEST_ID, *request_id);
                // An absent value is an absent key.
                if let Some(value) = value {
                    enc.write_field(field::metadata_value::VALUE, value);
                }
            }
            Self::MetadataKeys { request_id, keys } => {
                u32_field(enc, field::metadata_keys::REQUEST_ID, *request_id);
                // One positional `u32` count + strings, present even when empty.
                enc.write_field_with(field::metadata_keys::KEYS, |e| {
                    debug_assert!(u32::try_from(keys.len()).is_ok(), "key count exceeds u32");
                    e.write_u32_be(u32::try_from(keys.len()).unwrap_or(u32::MAX));
                    for key in keys {
                        e.write_str(key);
                    }
                });
            }
            Self::ListDirectory {
                request_id,
                path,
                host,
            } => encode_list_directory(enc, *request_id, path, host.as_ref()),
            Self::DirectoryListing { request_id, result } => {
                encode_directory_listing(enc, *request_id, result);
            }
            Self::PathQuery {
                request_id,
                root,
                query,
                recursive,
                host,
            } => encode_query(enc, *request_id, root, query, *recursive, host.as_ref()),
            Self::PathResults { request_id, result } => encode_results(enc, *request_id, result),
            _ => unreachable!("encode_payload routes only metadata frames here"),
        }
    }
}
