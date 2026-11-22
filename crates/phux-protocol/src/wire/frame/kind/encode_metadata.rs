//! Payload writers for the host directory and path queries. The L3
//! metadata frames are generated from their `field` modules.

use crate::wire::encode::Encoder;
use crate::wire::frame::directory::{encode_directory_listing, encode_list_directory};
use crate::wire::frame::path::{encode_query, encode_results};

use super::FrameKind;

impl FrameKind {
    /// Write a host-query frame's payload.
    pub(super) fn encode_metadata_payload(&self, enc: &mut Encoder<'_>) {
        match self {
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
