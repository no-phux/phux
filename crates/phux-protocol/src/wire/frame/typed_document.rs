//! The content type of a `Document` resource and the bootstrap that carries
//! it (`docs/resource-kinds.md`).
//!
//! A `Document` names its content type, a MIME type, once at
//! `SPAWN_RESOURCE` (in the `provider` field). A
//! [`DocumentTypedV1`](crate::caps::BootstrapStreamProfile::DocumentTypedV1)
//! bootstrap payload repeats it so a late viewer learns it: one length byte,
//! the content type, then the newest document (empty before the first
//! publish). Live `RESOURCE_OUTPUT` frames carry whole documents only.

/// Maximum bytes in a content type.
pub const MAX_CONTENT_TYPE_BYTES: usize = 255;

/// Whether `value` is a content type: ASCII `type/subtype` (RFC 6838
/// restricted names) with optional `;` parameters, at most
/// [`MAX_CONTENT_TYPE_BYTES`] bytes.
///
/// # Errors
///
/// What is wrong with it.
pub fn validate_content_type(value: &str) -> Result<(), &'static str> {
    if value.is_empty() || value.len() > MAX_CONTENT_TYPE_BYTES {
        return Err("a content type has 1-255 bytes");
    }
    if !value.bytes().all(|b| (0x20..0x7f).contains(&b)) {
        return Err("a content type is printable ASCII");
    }
    let name = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$&-^_.+".contains(&b))
    };
    match essence_of(value).split_once('/') {
        Some((ty, sub)) if name(ty) && name(sub) => Ok(()),
        _ => Err("a content type is type/subtype, e.g. text/markdown"),
    }
}

/// The `type/subtype` of a content type, without parameters or padding.
#[must_use]
pub fn essence_of(content_type: &str) -> &str {
    content_type.split(';').next().unwrap_or_default().trim()
}

/// Whether a document of `content_type` is text: `text/*`, JSON, and the
/// `+xml` and `+json` structured syntaxes, the families a publish must
/// carry as UTF-8.
#[must_use]
pub fn is_textual(content_type: &str) -> bool {
    let essence = essence_of(content_type).to_ascii_lowercase();
    essence.starts_with("text/")
        || essence == "application/json"
        || essence.ends_with("+xml")
        || essence.ends_with("+json")
}

/// The bootstrap payload of a `Document`: its content type behind a
/// one-byte length, then `document`.
///
/// # Panics
///
/// Never for a content type [`validate_content_type`] accepts; a longer
/// one is cut at 255 bytes.
#[must_use]
pub fn encode_bootstrap(content_type: &str, document: &[u8]) -> Vec<u8> {
    let head = &content_type.as_bytes()[..content_type.len().min(MAX_CONTENT_TYPE_BYTES)];
    let mut payload = Vec::with_capacity(1 + head.len() + document.len());
    payload.push(u8::try_from(head.len()).unwrap_or(u8::MAX));
    payload.extend_from_slice(head);
    payload.extend_from_slice(document);
    payload
}

/// Split a `Document` bootstrap payload into its content type and document,
/// or `None` when it is not one.
#[must_use]
pub fn split_bootstrap(payload: &[u8]) -> Option<(&str, &[u8])> {
    let (&len, rest) = payload.split_first()?;
    let len = usize::from(len);
    let head = rest.get(..len)?;
    let content_type = std::str::from_utf8(head).ok()?;
    validate_content_type(content_type).ok()?;
    Some((content_type, &rest[len..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_types_are_type_slash_subtype_with_optional_parameters() {
        for ok in [
            "text/markdown",
            "text/plain; charset=utf-8",
            "image/svg+xml",
            "application/vnd.phux.board+json",
        ] {
            assert_eq!(validate_content_type(ok), Ok(()), "{ok}");
        }
        for bad in [
            "",
            "text",
            "text/",
            "/plain",
            "text/plain/x",
            "text/plaïn",
            "text /plain",
            &format!("text/{}", "x".repeat(251)),
        ] {
            assert!(validate_content_type(bad).is_err(), "{bad}");
        }
        assert_eq!(essence_of("Text/Plain ; charset=utf-8"), "Text/Plain");
    }

    #[test]
    fn text_families_are_textual_and_images_are_not() {
        for text in [
            "text/markdown",
            "Text/CSV; charset=utf-8",
            "application/json",
            "image/svg+xml",
            "application/vnd.phux.board+json",
        ] {
            assert!(is_textual(text), "{text}");
        }
        for binary in ["image/png", "application/octet-stream", "application/pdf"] {
            assert!(!is_textual(binary), "{binary}");
        }
    }

    #[test]
    fn a_bootstrap_carries_the_content_type_before_the_document() {
        let payload = encode_bootstrap("text/markdown", b"# hi");
        assert_eq!(payload[0], 13);
        assert_eq!(
            split_bootstrap(&payload),
            Some(("text/markdown", &b"# hi"[..]))
        );
        let empty = encode_bootstrap("image/png", b"");
        assert_eq!(split_bootstrap(&empty), Some(("image/png", &b""[..])));
        assert_eq!(split_bootstrap(b""), None);
        assert_eq!(split_bootstrap(b"\x09text/"), None);
        assert_eq!(split_bootstrap(b"\x04abcd"), None);
    }
}
