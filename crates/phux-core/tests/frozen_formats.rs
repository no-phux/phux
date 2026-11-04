//! Formats this crate owns that outlive the image that wrote them. See
//! `phux_frozen` for what a check proves.

/// `receipts.receipt` in the organization store. Every `Op` variant, because
/// the store loads every row at startup and one it cannot decode stops the
/// server: a renamed variant must fail here, not on the first start after a
/// release. Sessions, resources, and arrangements use the same types as
/// `upgrade-blob` in `phux-server`, which freezes them.
#[test]
fn org_receipts() {
    phux_frozen::json::<Vec<phux_core::Receipt>>("org-receipts").check();
}
