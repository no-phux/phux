//! The machine-readable wire schema against the codec (ADR-0117).
//!
//! `docs/spec/wire-schema.json` lists every frame's type byte and, for each
//! top-level TLV field, its id, name, value type, and presence. Everything
//! except the value type is derived here from the codec itself: frame names
//! from the frame catalog (`wire/frame/catalog.rs`), field names and ids from
//! the `FIELDS` table `tlv_body!` emits in each `wire/field` module
//! (`field::FRAME_MODULES`), presence from encoding samples of every variant
//! and decoding each with one field stripped, and retired ids from the gaps
//! in a frame's id range. The rendered result must equal the committed file byte
//! for byte, so the codec cannot change shape without a schema diff in the
//! same change. Each declared value type is then checked against every
//! sampled value of its field.
//!
//! A new `FrameKind` variant fails to compile until it has a frame catalog
//! row, which gives it a name and an index, then fails [`every_variant_has_a_sample`] until it
//! has a sample. After an intended codec change, regenerate with
//! `PHUX_UPDATE_WIRE_SCHEMA=1 cargo nextest run -p phux-protocol wire_schema_matches`,
//! set the `type` of any new field (written as `?`), and review the diff.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use bytes::{Bytes, BytesMut};
use serde::{Deserialize, Serialize};

use super::{Decoder, decode_client_capabilities, decode_server_capabilities};
use crate::caps::{
    BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, Compression, ServerCapabilities,
};
use crate::ids::{
    BootstrapId, ClientId, IdempotencyKey, ResourceId, ServerInstance, SessionId, StreamId,
    WindowId,
};
use crate::wire::encode::wire_type;
use crate::wire::frame::{
    ActorRef, AgentEvent, CloseReason, CommandResult, CommandValue, DetachReason, DirectoryEntry,
    DirectoryErrorCode, DirectoryListing, DirectoryListingError, ErrorCode, EventStamp, FrameKind,
    HistoryRejectionReason, HistoryTombstoneReason, MoveResult, PathErrorCode, PathKind,
    PathQueryError, PathRow, PathStatus, Scope, SpawnResult, TYPE_FRAME_COMPRESSED,
    TombstoneReason,
};
use crate::wire::frame::{
    decode_actor_ref, decode_agent_event, decode_attach_target, decode_bootstrap_codec,
    decode_bootstrap_profile, decode_command, decode_command_result, decode_env, decode_key_event,
    decode_mouse_event, decode_move_result, decode_paste_event, decode_scope, decode_spawn_result,
    decode_string_list, decode_terminal_id, decode_viewport_info,
};
use crate::wire::info::SessionSnapshot;
use crate::wire::info::decode_session_snapshot;
use crate::wire::ssh_origin::decode_ssh_origin;

mod derive;
mod frames;
mod model;
mod roundtrip_tests;
mod samples;
mod stream_samples;
mod types;

use derive::{decodes, derive, fields, type_byte};
use frames::{FRAME_COMPRESSED, FRAME_VARIANTS, frame_entry};
use model::{FieldEntry, FrameEntry, Presence, Schema, render};
use samples::{Sample, actor, frame_samples, generation, key, samples, stream, terminal};
use stream_samples::{
    host_samples, lifecycle_samples, metadata_samples, reply_samples, spawn_samples, stream_samples,
};
use types::{KNOWN_TYPES, type_matches};

/// Repository-relative path of the schema.
const SCHEMA_PATH: &str = "docs/spec/wire-schema.json";

/// Set to regenerate the schema from the codec instead of checking it.
const UPDATE_ENV: &str = "PHUX_UPDATE_WIRE_SCHEMA";

/// Placeholder `type` the regenerator writes for a field it has not seen.
const UNSET_TYPE: &str = "?";

// ---------------------------------------------------------------------------
// Schema file model
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Frame catalog: names, field modules, variant coverage
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Samples
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// TLV body walk
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Derivation
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Value types
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

fn schema_path() -> PathBuf {
    // Read at run time, not `env!`: a baked checkout path defeats the shared
    // build cache (tools/src/policy/cache-portable.test.ts).
    PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("the test runner sets CARGO_MANIFEST_DIR"),
    )
    .join("../..")
    .join(SCHEMA_PATH)
}

fn committed() -> (String, Schema) {
    let text = std::fs::read_to_string(schema_path())
        .unwrap_or_else(|err| panic!("cannot read {SCHEMA_PATH}: {err}"));
    let schema = serde_json::from_str(&text)
        .unwrap_or_else(|err| panic!("{SCHEMA_PATH} is not a valid schema: {err}"));
    (text, schema)
}

/// The first line where `a` and `b` differ, for a readable failure.
fn first_difference(a: &str, b: &str) -> String {
    a.lines()
        .zip(b.lines())
        .enumerate()
        .find(|(_, (x, y))| x != y)
        .map_or_else(
            || "the files differ in length".to_owned(),
            |(n, (x, y))| format!("line {}:\n  committed: {x}\n  codec:     {y}", n + 1),
        )
}

#[test]
fn wire_schema_matches_the_codec() {
    let (text, current) = committed();
    let rendered = render(&derive(&current));
    if std::env::var_os(UPDATE_ENV).is_some() {
        std::fs::write(schema_path(), &rendered).unwrap();
        return;
    }
    assert!(
        text == rendered,
        "{SCHEMA_PATH} disagrees with the codec at {}\n\
         If the codec change is intended, regenerate with \
         `{UPDATE_ENV}=1 cargo nextest run -p phux-protocol wire_schema_matches`, set the `type` of \
         any new field, update the spec prose, and review the diff.",
        first_difference(&text, &rendered)
    );
}

#[test]
fn every_variant_has_a_sample() {
    let covered: BTreeSet<usize> = frame_samples()
        .iter()
        .map(|frame| frame_entry(frame).0)
        .collect();
    assert_eq!(
        covered,
        (0..FRAME_VARIANTS).collect(),
        "every FrameKind variant needs a sample: client frames in kinds/samples, server frames in wire_schema/"
    );
}

#[test]
fn spec_names_are_unique() {
    let mut rows: BTreeMap<&str, usize> = BTreeMap::new();
    for frame in frame_samples() {
        let (row, name, _) = frame_entry(&frame);
        let first = *rows.entry(name).or_insert(row);
        assert_eq!(first, row, "{name} names catalog rows {first} and {row}");
    }
    assert!(!rows.contains_key(FRAME_COMPRESSED.0));
}

#[test]
fn every_server_frame_is_denied_as_wrong_direction() {
    for frame in frame_samples() {
        if frame.type_byte() & 0x80 != 0 {
            crate::kinds::samples::assert_wrong_direction_denied(&frame);
        }
    }
}

#[test]
fn samples_carry_only_length_delimited_unrepeated_fields() {
    for sample in samples() {
        let tlvs = fields(&sample.bytes);
        for tlv in &tlvs {
            assert_eq!(
                tlv.wire_type,
                wire_type::BYTES,
                "{}.{}",
                sample.name,
                tlv.id
            );
        }
        let ids: BTreeSet<u32> = tlvs.iter().map(|tlv| tlv.id).collect();
        assert_eq!(ids.len(), tlvs.len(), "{} repeats a field", sample.name);
    }
    let compressed = samples()
        .into_iter()
        .any(|sample| type_byte(&sample.bytes) == TYPE_FRAME_COMPRESSED);
    assert!(compressed, "the FRAME_COMPRESSED sample was not compressed");
}

#[test]
fn declared_types_match_every_sampled_value() {
    let (_, schema) = committed();
    for sample in samples() {
        let frame = schema
            .frames
            .iter()
            .find(|frame| frame.name == sample.name)
            .unwrap_or_else(|| panic!("{} missing from {SCHEMA_PATH}", sample.name));
        for tlv in fields(&sample.bytes) {
            let field = frame.fields.iter().find(|f| f.id == tlv.id).unwrap();
            let value = &sample.bytes[tlv.value.clone()];
            let matches = type_matches(&field.ty, value).unwrap_or_else(|| {
                panic!("{}.{}: unknown type `{}`", frame.name, field.name, field.ty)
            });
            assert!(
                matches,
                "{}.{} is declared `{}` but carries {value:02x?}",
                frame.name, field.name, field.ty
            );
        }
    }
}

#[test]
fn declared_types_are_exactly_the_checked_ones() {
    let (_, schema) = committed();
    let known: BTreeSet<&str> = KNOWN_TYPES.iter().copied().collect();
    let documented: BTreeSet<&str> = schema.types.keys().map(String::as_str).collect();
    let used: BTreeSet<&str> = schema
        .frames
        .iter()
        .flat_map(|frame| &frame.fields)
        .map(|field| field.ty.as_str())
        .collect();
    assert_eq!(
        documented, known,
        "`types` must document exactly the checked types"
    );
    assert_eq!(
        used, known,
        "every checked type must be used, and nothing else"
    );
    for name in known {
        assert!(type_matches(name, &[]).is_some(), "{name} has no checker");
    }
}

#[test]
fn every_frame_module_is_a_sampled_frame() {
    let mut frames: BTreeSet<&str> = frame_samples()
        .iter()
        .map(|frame| frame_entry(frame).2)
        .filter(|module| !module.is_empty())
        .collect();
    frames.insert(FRAME_COMPRESSED.1);
    let listed: BTreeSet<&str> = crate::wire::field::FRAME_MODULES
        .iter()
        .map(|(name, _)| *name)
        .collect();
    assert_eq!(
        listed, frames,
        "field::FRAME_MODULES must list exactly the sampled frames' modules"
    );
}
