//! Frozen upgrade blobs: what an already-released image hands this one.
//!
//! Field names are the format, and the reader cannot refuse after the
//! re-exec, so `tests/fixtures/upgrade-blob/` is append-only
//! (`scripts/check-frozen-fixtures.sh`). `phux_frozen` holds this image to
//! them: every fixture reads back with nothing lost, the newest re-serializes
//! exactly, and the newest gives every field a value.
//!
//! To freeze a change, `PHUX_BLESS_FROZEN=upgrade-blob cargo nextest run -p
//! phux-server --lib upgrade::blob::golden` writes the next fixture.

use phux_frozen::Format;
use serde_json::Value;

use super::*;

fn format() -> Format<'static> {
    phux_frozen::custom_json("upgrade-blob", |bytes| {
        let blob = StateBlob::from_bytes(bytes).map_err(|error| error.to_string())?;
        let written = blob.to_bytes().map_err(|error| error.to_string())?;
        serde_json::from_slice(&written).map_err(|error| error.to_string())
    })
    .in_dir("tests/fixtures/upgrade-blob")
}

#[test]
fn golden_blobs_hold_every_released_shape() {
    format().check();
}

#[test]
fn golden_blobs_pass_the_preflight_a_resume_depends_on() {
    for (name, bytes) in format().fixtures() {
        preflight(&bytes).unwrap_or_else(|reason| panic!("{name}: {reason}"));
    }
}

#[test]
fn preflight_refuses_records_a_resume_would_die_on() {
    let (_, bytes) = format().fixtures().pop().expect("a fixture");
    let mut blob: Value = serde_json::from_slice(&bytes).expect("json");
    blob["org"]["resources"][0]["session"] = blob["org"]["authority"]["id"].clone();
    let broken = serde_json::to_vec(&blob).expect("serialize");
    let reason = preflight(&broken).expect_err("a resource outside any session");
    assert!(reason.contains("organization"), "{reason}");
}
