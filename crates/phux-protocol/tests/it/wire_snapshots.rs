//! The byte goldens of every wire-snapshot family, checked by one loop.
//!
//! Each family keeps its fixtures in its own module. A fixture's `.snap`
//! file is still named for that module (`it__<module>__<fixture>.snap`), so
//! the goldens did not move when the families stopped running their own
//! loops. Goldens are authored fixtures, never derived from the sample
//! corpus.

use crate::common::dump_frame;
use phux_protocol::wire::frame::FrameKind;

type Fixtures = Vec<(&'static str, FrameKind)>;

/// Each family's module, which prefixes its `.snap` files, and its fixtures.
fn families() -> [(&'static str, Fixtures); 5] {
    [
        (
            "frame_wire_snapshots",
            crate::frame_wire_snapshots::fixtures(),
        ),
        ("org_wire_snapshots", crate::org_wire_snapshots::fixtures()),
        ("forward_snapshots", crate::forward_snapshots::fixtures()),
        (
            "service_http_snapshots",
            crate::service_http_snapshots::fixtures(),
        ),
        (
            "file_transfer_snapshots",
            crate::file_transfer_snapshots::fixtures(),
        ),
    ]
}

/// Every fixture decodes back to itself and its hex dump matches its
/// committed golden byte-for-byte.
#[test]
fn wire_snapshots_match_goldens() {
    let mut settings = insta::Settings::clone_current();
    settings.set_prepend_module_to_snapshot(false);
    settings.bind(|| {
        for (module, fixtures) in families() {
            for (name, frame) in fixtures {
                let snapshot = format!("it__{module}__{name}");
                insta::assert_snapshot!(snapshot, dump_frame(&frame));
            }
        }
    });
}
