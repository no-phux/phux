//! CONNECT header uniqueness for the WebTransport accept path (phux-50wm).
//!
//! `wtransport` 0.7.2 builds `SessionRequest` from a `HashMap`, and its QPACK
//! decoder inserts field lines with `HashMap::insert`. Repeated identical
//! lowercase `authorization` fields therefore collapse before application
//! code can see them. This scanner walks the QPACK field list *before* any
//! map is built and reports a second `Authorization` (case-insensitive).

use wtransport_proto::bytes::BytesReader;
use wtransport_proto::qpack::DecodingError;

/// QPACK static-table index of `authorization` (RFC 9204 Appendix A).
const AUTHORIZATION_STATIC_INDEX: usize = 84;

/// Whether `payload` encodes more than one `Authorization` field line.
pub(super) fn has_duplicate_authorization(payload: &[u8]) -> Result<bool, DecodingError> {
    let mut reader = payload;
    decode_integer(&mut reader, 8)?;
    decode_integer(&mut reader, 7)?;

    let mut seen_authorization = false;
    while !reader.is_empty() {
        if field_is_authorization(&mut reader)? {
            if seen_authorization {
                return Ok(true);
            }
            seen_authorization = true;
        }
    }
    Ok(false)
}

fn field_is_authorization(reader: &mut &[u8]) -> Result<bool, DecodingError> {
    let first = *reader.first().ok_or(DecodingError::UnexpectedFin)?;
    match field_line_type(first) {
        FieldLineType::Indexed => {
            let is_dynamic = first & 0b0100_0000 == 0;
            if is_dynamic {
                return Err(DecodingError::DynamicNotSupported);
            }
            let index = decode_integer(reader, 6)?.1;
            Ok(index == AUTHORIZATION_STATIC_INDEX)
        }
        FieldLineType::IndexedPost | FieldLineType::LiteralPostRefName => {
            Err(DecodingError::DynamicNotSupported)
        }
        FieldLineType::LiteralRefName => {
            let is_dynamic = first & 0b0001_0000 == 0;
            if is_dynamic {
                return Err(DecodingError::DynamicNotSupported);
            }
            let index = decode_integer(reader, 4)?.1;
            skip_string(reader, 7)?;
            Ok(index == AUTHORIZATION_STATIC_INDEX)
        }
        FieldLineType::LiteralLitName => {
            let name = decode_string(reader, 3)?;
            skip_string(reader, 7)?;
            Ok(name.eq_ignore_ascii_case("authorization"))
        }
    }
}

enum FieldLineType {
    Indexed,
    IndexedPost,
    LiteralRefName,
    LiteralPostRefName,
    LiteralLitName,
}

const fn field_line_type(byte: u8) -> FieldLineType {
    if byte >> 7 == 1 {
        FieldLineType::Indexed
    } else if byte >> 4 == 1 {
        FieldLineType::IndexedPost
    } else if byte >> 6 == 1 {
        FieldLineType::LiteralRefName
    } else if byte >> 4 == 0 {
        FieldLineType::LiteralPostRefName
    } else {
        FieldLineType::LiteralLitName
    }
}

fn decode_integer(reader: &mut &[u8], prefix_bits: usize) -> Result<(u8, usize), DecodingError> {
    let byte = reader.get_bytes(1).ok_or(DecodingError::UnexpectedFin)?[0] as usize;
    let mask = (1_usize << prefix_bits) - 1;
    let flags = u8::try_from(byte >> prefix_bits).unwrap_or(0);
    let mut value = byte & mask;
    if value != mask {
        return Ok((flags, value));
    }
    let mut power = 0;
    loop {
        let byte = reader.get_bytes(1).ok_or(DecodingError::UnexpectedFin)?[0] as usize;
        value = value
            .checked_add((byte & 0x7F) << power)
            .ok_or(DecodingError::IntegerOverflow)?;
        power += 7;
        if byte & 0x80 == 0 {
            break;
        }
    }
    Ok((flags, value))
}

fn skip_string(reader: &mut &[u8], prefix_bits: usize) -> Result<(), DecodingError> {
    let (_, len) = decode_integer(reader, prefix_bits)?;
    reader.get_bytes(len).ok_or(DecodingError::UnexpectedFin)?;
    Ok(())
}

fn decode_string(reader: &mut &[u8], prefix_bits: usize) -> Result<String, DecodingError> {
    let (flags, len) = decode_integer(reader, prefix_bits)?;
    let data = reader.get_bytes(len).ok_or(DecodingError::UnexpectedFin)?;
    let decoded = if flags & 1 == 1 {
        let mut out = Vec::with_capacity(len);
        httlib_huffman::decode(data, &mut out, httlib_huffman::DecoderSpeed::OneBit)
            .map_err(|_| DecodingError::InvalidString)?;
        out
    } else {
        data.to_vec()
    };
    String::from_utf8(decoded).map_err(|_| DecodingError::InvalidString)
}

#[cfg(test)]
mod tests {
    use super::has_duplicate_authorization;
    use wtransport_proto::qpack::Encoder;

    #[test]
    fn one_authorization_is_not_duplicate() {
        let payload = Encoder::encode([("authorization", "Bearer abc")]);
        assert!(!has_duplicate_authorization(&payload).unwrap());
    }

    #[test]
    fn repeated_lowercase_authorization_is_duplicate() {
        let payload = Encoder::encode([
            ("authorization", "Bearer abc"),
            ("authorization", "Bearer abc"),
        ]);
        assert!(has_duplicate_authorization(&payload).unwrap());
    }

    #[test]
    fn case_distinct_authorization_is_duplicate() {
        let payload = Encoder::encode([
            ("Authorization", "Bearer abc"),
            ("authorization", "Bearer abc"),
        ]);
        assert!(has_duplicate_authorization(&payload).unwrap());
    }

    #[test]
    fn missing_authorization_is_not_duplicate() {
        let payload = Encoder::encode([(":path", "/session"), (":method", "CONNECT")]);
        assert!(!has_duplicate_authorization(&payload).unwrap());
    }

    #[test]
    fn collapsed_hashmap_cannot_see_the_duplicate() {
        let payload = Encoder::encode([
            ("authorization", "Bearer abc"),
            ("authorization", "Bearer abc"),
        ]);
        let decoded = wtransport_proto::qpack::Decoder::decode(&payload).unwrap();
        assert_eq!(decoded.len(), 1, "dependency map collapses identical names");
        assert!(has_duplicate_authorization(&payload).unwrap());
    }
}
