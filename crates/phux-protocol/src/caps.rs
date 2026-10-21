//! Capability advertisements (SPEC §6.2).
//!
//! Capabilities live in HELLO and apply for the life of the connection. The
//! server rewrites outbound SGR bytes to the advertised [`ColorSupport`]
//! (ADR-0013).

include!("caps/transport.rs");
include!("caps/bootstrap.rs");
include!("caps/server_features.rs");
include!("caps/advertisements.rs");

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_map(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |key: &str| map.get(key).cloned()
    }

    #[test]
    fn color_support_wire_roundtrips_every_variant() {
        for v in [
            ColorSupport::TrueColor,
            ColorSupport::Indexed256,
            ColorSupport::Indexed16,
            ColorSupport::Mono,
        ] {
            let tag = v.as_wire();
            let back = ColorSupport::from_wire(tag).expect("known tag");
            assert_eq!(back, v);
        }
        assert!(ColorSupport::from_wire(0xFF).is_none());
    }

    #[test]
    fn protocol_sets_ignore_unknown_bits() {
        assert_eq!(ImageProtocolSet::from_wire(0xFF), ImageProtocolSet::all());
        assert_eq!(ImageProtocolSet::all().as_wire(), 0x07);
        assert_eq!(
            KeyboardProtocolSet::from_wire(0xFF),
            KeyboardProtocolSet::all()
        );
        assert_eq!(KeyboardProtocolSet::all().as_wire(), 0x03);
    }

    #[test]
    fn color_support_detection_from_env() {
        let cases: &[(&[(&str, &str)], ColorSupport)] = &[
            (
                &[("COLORTERM", "truecolor"), ("TERM", "xterm-256color")],
                ColorSupport::TrueColor,
            ),
            (
                &[("COLORTERM", "24bit"), ("TERM", "xterm")],
                ColorSupport::TrueColor,
            ),
            (&[("TERM", "xterm-256color")], ColorSupport::Indexed256),
            (&[("TERM", "xterm-direct")], ColorSupport::TrueColor),
            (&[("TERM", "xterm")], ColorSupport::Indexed16),
            (&[("TERM", "dumb")], ColorSupport::Mono),
            (
                &[("TERM_PROGRAM", "Apple_Terminal")],
                ColorSupport::Indexed256,
            ),
            (&[("TERM_PROGRAM", "iTerm.app")], ColorSupport::TrueColor),
            (&[], ColorSupport::TrueColor),
        ];
        for (pairs, want) in cases {
            assert_eq!(detect_from_env(env_map(pairs)), *want, "{pairs:?}");
        }
    }

    #[test]
    fn native_advertisement_is_opt_in_and_indivisible() {
        let synth = BootstrapProfileSet::with(&[
            BootstrapProfileKind::SynthesizedVtRaw,
            BootstrapProfileKind::SynthesizedVtStateSync,
        ]);
        let base = BootstrapCapabilities::new();
        assert_eq!(base.profiles, synth);
        assert_eq!(base.native_codecs.as_wire(), 0);
        assert_eq!(base.native_features.as_wire(), 0);

        let native = base.with_native(
            EngineCodec::LibghosttyCheckpointV2,
            EngineFeatureSet::required_native(),
        );
        assert!(native.profiles.contains(BootstrapProfileKind::NativeState));
        assert!(
            native
                .profiles
                .contains(BootstrapProfileKind::SynthesizedVtRaw)
        );
        assert!(
            native
                .native_codecs
                .contains(EngineCodec::LibghosttyCheckpointV2)
        );
        assert_eq!(native.native_features.as_wire(), 0x0000_000f);

        let partial =
            EngineFeatureSet::with(&[EngineFeature::Continuation, EngineFeature::ReadyBoundary]);
        let withdrawn = native.with_native(EngineCodec::LibghosttyCheckpointV2, partial);
        assert_eq!(withdrawn, base);
    }

    /// Every variant has exactly one name, each bit is unique, and the
    /// known mask is their union.
    #[test]
    fn every_server_feature_has_one_name_and_a_unique_bit() {
        let mut union = 0_u32;
        let mut names = Vec::new();
        let mut snakes = Vec::new();
        for &feature in ServerFeature::ALL {
            let name = feature.wire_name();
            let snake = feature.snake_name();
            let bits = feature as u32;
            assert_eq!(bits.count_ones(), 1, "{name} must be a single bit");
            assert_eq!(union & bits, 0, "{name} reuses an allocated bit");
            union |= bits;
            assert_eq!(
                snake,
                name.to_ascii_lowercase(),
                "{name} snake_case is not its constant lower-cased"
            );
            assert!(!names.contains(&name), "{name} is declared more than once");
            assert!(
                !snakes.contains(&snake),
                "{snake} is declared more than once"
            );
            names.push(name);
            snakes.push(snake);
        }
        assert_eq!(names.len(), ServerFeature::ALL.len());
        assert_eq!(snakes.len(), ServerFeature::ALL.len());
        assert_eq!(ServerFeatureSet::all().as_wire(), union);
        assert_eq!(ServerFeatureSet::from_wire(u32::MAX).as_wire(), union);
        // ADR-0137: word 0 stays closed. These masks are the inventory of
        // holes (never assigned, retired-unshipped, reserved-unallocated).
        assert_eq!(union & 0x0000_000F, 0, "low nibble stays unassigned");
        assert_eq!(union & 0x0000_1000, 0, "0x1000 stays retired-unshipped");
        assert_eq!(union & 0x8000_0000, 0, "0x80000000 stays unallocated");
    }
}
