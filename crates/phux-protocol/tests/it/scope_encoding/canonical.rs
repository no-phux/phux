//! The §5 canonical bytes and every refusal rule for them.

use super::{grant, host, set};
use phux_protocol::scope::{EffectiveScopeSet, MAX_GRANTS, ScopeError, Selector, TerminalScopeSet};

#[test]
fn scope_set_canonical_bytes_match_spec_section_5_goldens() {
    let selectors: [(Selector, &[u8]); 6] = [
        (Selector::Global, &[0x00]),
        (Selector::HostLocal, &[0x01, 0x00]),
        (
            Selector::HostSatellite(host("ab")),
            &[0x01, 0x01, 0x00, 0x02, b'a', b'b'],
        ),
        (Selector::Group(7), &[0x02, 0x00, 0x00, 0x00, 0x07]),
        (
            Selector::TerminalLocal(9),
            &[0x03, 0x00, 0x00, 0x00, 0x00, 0x09],
        ),
        (
            Selector::TerminalSatellite(host("ab"), 3),
            &[0x03, 0x01, 0x00, 0x02, b'a', b'b', 0x00, 0x00, 0x00, 0x03],
        ),
    ];
    for (selector, bytes) in selectors {
        assert_eq!(selector.to_bytes(), bytes, "{selector}");
        assert_eq!(Selector::decode(bytes).unwrap(), selector, "{selector}");
    }

    // Grants sort by selector bytes, whatever order they were written in.
    let scopes = set(&["observe@terminal:9", "*@global"]);
    let golden = [
        0x00, 0x02, // grant_count
        0x00, 0x01, 0x00, 0xFF, // GLOBAL, all eight verbs
        0x00, 0x06, 0x03, 0x00, 0x00, 0x00, 0x00, 0x09, 0x02, // TERMINAL_LOCAL(9), OBSERVE
    ];
    assert_eq!(scopes.encode(), golden);
    assert_eq!(TerminalScopeSet::decode(&golden).unwrap(), scopes);

    // Equal selectors merge by OR-ing their verbs; the encoder emits one.
    let merged = set(&["observe@group:7", "bind@group:7"]);
    assert_eq!(
        merged.encode(),
        [0x00, 0x01, 0x00, 0x05, 0x02, 0x00, 0x00, 0x00, 0x07, 0x0A]
    );

    // An effective clause carries both selectors.
    let effective = EffectiveScopeSet::intersect(
        &set(&["bind,observe@group:7"]),
        &set(&["observe@terminal:9"]),
    )
    .unwrap();
    let clause_golden = [
        0x00, 0x01, // clause_count
        0x00, 0x05, 0x02, 0x00, 0x00, 0x00, 0x07, // requested GROUP(7)
        0x00, 0x06, 0x03, 0x00, 0x00, 0x00, 0x00, 0x09, // ceiling TERMINAL_LOCAL(9)
        0x02, // OBSERVE
    ];
    assert_eq!(effective.encode(), clause_golden);
    assert_eq!(
        EffectiveScopeSet::decode(&clause_golden).unwrap(),
        effective
    );
}

#[test]
fn scope_set_rejects_unsorted_duplicate_unknown_bit_nonminimal_and_trailing() {
    let cases: [(&str, Vec<u8>, ScopeError); 11] = [
        (
            "unsorted",
            vec![
                0x00, 0x02, 0x00, 0x06, 0x03, 0x00, 0x00, 0x00, 0x00, 0x09, 0x02, 0x00, 0x01, 0x00,
                0x3F,
            ],
            ScopeError::Unsorted,
        ),
        (
            "duplicate",
            vec![0x00, 0x02, 0x00, 0x01, 0x00, 0x02, 0x00, 0x01, 0x00, 0x04],
            ScopeError::Duplicate,
        ),
        (
            "zero verbs",
            vec![0x00, 0x01, 0x00, 0x01, 0x00, 0x00],
            ScopeError::ZeroVerbs,
        ),
        (
            "non-minimal selector length",
            vec![0x00, 0x01, 0x00, 0x02, 0x00, 0x00, 0x02],
            ScopeError::NonMinimalSelector,
        ),
        (
            "trailing byte",
            vec![0x00, 0x01, 0x00, 0x01, 0x00, 0x02, 0xFF],
            ScopeError::TrailingBytes,
        ),
        (
            "count larger than the grants present",
            vec![0x00, 0x02, 0x00, 0x01, 0x00, 0x02],
            ScopeError::Truncated,
        ),
        (
            "truncated selector",
            vec![0x00, 0x01, 0x00, 0x05, 0x02, 0x00],
            ScopeError::Truncated,
        ),
        (
            "unknown selector tag",
            vec![0x00, 0x01, 0x00, 0x01, 0x06, 0x02],
            ScopeError::UnknownSelectorTag,
        ),
        (
            "unknown subtype",
            vec![0x00, 0x01, 0x00, 0x02, 0x01, 0x02, 0x02],
            ScopeError::UnknownSubtype,
        ),
        (
            "empty host",
            vec![0x00, 0x01, 0x00, 0x04, 0x01, 0x01, 0x00, 0x00, 0x02],
            ScopeError::InvalidHost,
        ),
        (
            "more than 64 grants",
            vec![0x00, 0x41],
            ScopeError::TooManyGrants,
        ),
    ];
    for (name, image, expected) in cases {
        assert_eq!(
            TerminalScopeSet::decode(&image),
            Err(expected),
            "{name}: {image:02x?}"
        );
    }

    // A valid image with one trailing byte is refused, not trimmed, and the
    // same holds for an effective set.
    let effective = EffectiveScopeSet::intersect(&set(&["*@global"]), &set(&["*@global"])).unwrap();
    let mut image = effective.encode();
    image.push(0x00);
    assert_eq!(
        EffectiveScopeSet::decode(&image),
        Err(ScopeError::TrailingBytes)
    );
    assert_eq!(
        EffectiveScopeSet::decode(&[0x00, 0x41]),
        Err(ScopeError::TooManyClauses)
    );

    // The encoder refuses more than 64 distinct selectors outright.
    let too_many =
        (0..=u32::try_from(MAX_GRANTS).unwrap()).map(|id| grant(&format!("observe@terminal:{id}")));
    assert_eq!(
        TerminalScopeSet::from_grants(too_many),
        Err(ScopeError::TooManyGrants)
    );
}
