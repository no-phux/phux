//! `defaults.session-name-template` rendering: `${cwd-basename}` and the
//! generated `${random-name}` (`drifting-cedar`).
//!
//! Generated names are lowercase ASCII `adjective-noun`, so always a valid
//! bare session name. The picker is a non-cryptographic `SplitMix64`: these
//! are display labels, and callers enforce uniqueness.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::path::Path;

/// The template placeholder that expands to a generated adjective-noun name.
pub const RANDOM_NAME_PLACEHOLDER: &str = "${random-name}";

/// Curated adjectives: lowercase ASCII, inoffensive, no people or brands.
#[rustfmt::skip] // a word grid, not one word per line
pub(crate) const ADJECTIVES: &[&str] = &[
    "amber", "ancient", "autumn", "bold", "brave", "breezy", "bright", "brisk", "calm", "candid",
    "clever", "cosmic", "crisp", "curious", "dappled", "dawning", "deep", "drifting", "eager",
    "early", "easy", "electric", "emerald", "evening", "fleet", "floating", "fluent", "gentle",
    "gilded", "glad", "gleaming", "golden", "grand", "hidden", "hollow", "humble", "hushed", "icy",
    "jolly", "keen", "kind", "lively", "lucid", "lunar", "mellow", "merry", "misty", "modest",
    "morning", "mossy", "nimble", "noble", "patient", "placid", "polished", "proud", "quick",
    "quiet", "rapid", "restless", "rolling", "rustic", "rustling", "sandy", "silent", "silver",
    "sleepy", "snowy", "solar", "spry", "steady", "still", "stormy", "sturdy", "sunny", "swift",
    "tender", "tidal", "tranquil", "twilight", "velvet", "vivid", "wandering", "warm", "wild",
    "windy", "winter", "wise", "woven", "young", "zesty", "azure",
];

/// Curated nouns: lowercase ASCII, inoffensive, no people or brands.
#[rustfmt::skip] // a word grid, not one word per line
pub(crate) const NOUNS: &[&str] = &[
    "acorn", "alder", "anchor", "arbor", "badger", "bamboo", "basin", "beacon", "birch", "bluff",
    "boulder", "brook", "canyon", "cedar", "cinder", "comet", "coral", "cove", "crane", "creek",
    "delta", "dune", "eagle", "ember", "falcon", "fern", "field", "fjord", "forest", "fox",
    "garden", "glacier", "glade", "grove", "harbor", "hawk", "heron", "hill", "island", "juniper",
    "kestrel", "lagoon", "lake", "lantern", "larch", "lark", "ledge", "lichen", "lotus", "maple",
    "marsh", "meadow", "mesa", "moss", "moth", "nebula", "oak", "orchard", "otter", "owl",
    "pebble", "pine", "planet", "plover", "pond", "prairie", "quail", "rain", "raven", "reef",
    "ridge", "river", "sparrow", "spruce", "star", "stone", "stream", "summit", "thicket",
    "thistle", "tide", "trail", "tundra", "valley", "wren", "yarrow", "zephyr", "orbit", "canopy",
    "pier",
];

/// A small deterministic name picker (`SplitMix64`).
#[derive(Debug, Clone)]
pub struct NameRng {
    state: u64,
}

impl NameRng {
    /// A generator that yields the same picks for the same seed.
    #[must_use]
    pub const fn seeded(seed: u64) -> Self {
        Self { state: seed }
    }

    /// A generator seeded from std's per-process random hash keys, mixed
    /// with the clock and pid so two invocations in one process differ too.
    #[must_use]
    pub fn from_entropy() -> Self {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u32(std::process::id());
        if let Ok(elapsed) = std::time::UNIX_EPOCH.elapsed() {
            hasher.write_u128(elapsed.as_nanos());
        }
        Self::seeded(hasher.finish())
    }

    const fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// One word from a non-empty list (modulo bias is irrelevant here).
    fn pick(&mut self, words: &[&'static str]) -> &'static str {
        let len = u64::try_from(words.len()).unwrap_or(u64::MAX);
        let index = usize::try_from(self.next_u64() % len).unwrap_or(0);
        words[index]
    }
}

/// One generated `adjective-noun` name, e.g. `drifting-cedar`.
#[must_use]
pub fn random_name(rng: &mut NameRng) -> String {
    let adjective = rng.pick(ADJECTIVES);
    let noun = rng.pick(NOUNS);
    format!("{adjective}-{noun}")
}

/// Whether `template` asks for a generated name, so re-rendering can
/// resolve a collision.
#[must_use]
pub fn template_has_random_name(template: &str) -> bool {
    template.contains(RANDOM_NAME_PLACEHOLDER)
}

/// Render a session-name template for an auto-created session.
///
/// `${cwd-basename}` becomes the last component of `cwd` with `:` replaced by
/// `_` (`:` is the selector's session/window delimiter); `${random-name}` a
/// fresh generated name. Unknown placeholders pass through. May return an
/// empty string (`/` has no basename); the caller picks the fallback.
#[must_use]
pub fn render_session_name_template(template: &str, cwd: &Path) -> String {
    render_session_name_template_with(template, cwd, &mut NameRng::from_entropy())
}

/// [`render_session_name_template`] with a caller-supplied generator. Every
/// `${random-name}` in one render is the same pick, expanded before
/// `${cwd-basename}` so a directory name is never re-expanded.
#[must_use]
pub fn render_session_name_template_with(template: &str, cwd: &Path, rng: &mut NameRng) -> String {
    let basename = cwd
        .file_name()
        .map(|os| os.to_string_lossy().replace(':', "_"))
        .unwrap_or_default();
    let expanded = if template_has_random_name(template) {
        template.replace(RANDOM_NAME_PLACEHOLDER, &random_name(rng))
    } else {
        template.to_owned()
    };
    expanded.replace("${cwd-basename}", &basename)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn word_lists_are_curated_and_disjoint() {
        for list in [ADJECTIVES, NOUNS] {
            assert!((64..=128).contains(&list.len()));
            assert!(
                list.iter()
                    .all(|w| !w.is_empty() && w.bytes().all(|b| b.is_ascii_lowercase()))
            );
            assert_eq!(list.iter().collect::<BTreeSet<_>>().len(), list.len());
        }
        let adjectives: BTreeSet<_> = ADJECTIVES.iter().collect();
        assert!(NOUNS.iter().all(|n| !adjectives.contains(n)));
    }

    #[test]
    fn seeded_picks_are_deterministic_well_formed_and_varied() {
        let (mut a, mut b) = (NameRng::seeded(42), NameRng::seeded(42));
        let names: BTreeSet<_> = (0..64)
            .map(|_| {
                let name = random_name(&mut a);
                assert_eq!(name, random_name(&mut b));
                let (adjective, noun) = name.split_once('-').expect("adjective-noun");
                assert!(ADJECTIVES.contains(&adjective) && NOUNS.contains(&noun));
                name
            })
            .collect();
        assert!(names.len() > 32, "picks barely vary: {names:?}");
    }

    #[test]
    fn templates_render_per_their_placeholders() {
        let seeded = random_name(&mut NameRng::seeded(9));
        for (template, cwd, want) in [
            ("default", "/Users/me/phux", "default".to_owned()),
            (
                "phux-${cwd-basename}",
                "/Users/me/phux",
                "phux-phux".to_owned(),
            ),
            ("${cwd-basename}", "/tmp/a:b", "a_b".to_owned()),
            (
                "${cwd-basename}",
                "/tmp/my.project",
                "my.project".to_owned(),
            ),
            ("${cwd-basename}", "/", String::new()),
            ("${unknown}", "/tmp/x", "${unknown}".to_owned()),
            ("${random-name}", "/tmp/x", seeded.clone()),
            (
                "${cwd-basename}-${random-name}",
                "/home/notes",
                format!("notes-{seeded}"),
            ),
            (
                "${random-name}/${random-name}",
                "/tmp/x",
                format!("{seeded}/{seeded}"),
            ),
            // A basename containing the placeholder is not re-expanded.
            (
                "${cwd-basename}",
                "/tmp/${random-name}",
                "${random-name}".to_owned(),
            ),
        ] {
            let got = render_session_name_template_with(
                template,
                Path::new(cwd),
                &mut NameRng::seeded(9),
            );
            assert_eq!(got, want, "{template} in {cwd}");
        }
        assert!(template_has_random_name("work-${random-name}"));
        assert!(!template_has_random_name("${cwd-basename}"));

        // A template without the placeholder draws nothing from the rng.
        let mut rng = NameRng::seeded(11);
        let _ = render_session_name_template_with("default", Path::new("/tmp/x"), &mut rng);
        assert_eq!(random_name(&mut rng), random_name(&mut NameRng::seeded(11)));
    }
}
