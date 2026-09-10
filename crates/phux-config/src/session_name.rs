//! Generated human-readable session names for the `${random-name}`
//! placeholder in `defaults.session-name-template` (phux-c2td.6).
//!
//! A generated name is one adjective and one noun from two small embedded
//! word lists, joined by `-` (`drifting-cedar`). The words are lowercase
//! ASCII letters only, so a generated name is always a valid bare session
//! name and never collides with the `name:N.M` selector grammar.
//!
//! The picker is a non-cryptographic `SplitMix64` generator. Nothing
//! security-relevant rides on these names: they are display labels, the
//! session's stable identity is its server-assigned id, and uniqueness is
//! enforced by the caller against the live session list. Seeding from std's
//! per-process hash keys keeps this free of a randomness dependency, and an
//! explicit seed keeps every test deterministic.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};

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
///
/// Clone it to preview the picks a generator will make; tests seed it with
/// [`NameRng::seeded`] and production code uses [`NameRng::from_entropy`].
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

    /// One word from a non-empty list. The modulo bias over lists this
    /// small is far below anything a display name could notice.
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

/// Whether `template` asks for a generated name, so a caller resolving a
/// collision knows that re-rendering can produce a different candidate.
#[must_use]
pub fn template_has_random_name(template: &str) -> bool {
    template.contains(RANDOM_NAME_PLACEHOLDER)
}

#[cfg(test)]
mod tests {
    use super::{ADJECTIVES, NOUNS, NameRng, random_name, template_has_random_name};
    use std::collections::BTreeSet;

    fn assert_curated(list: &[&str]) {
        assert!(
            (64..=128).contains(&list.len()),
            "list size {} outside 64..=128",
            list.len()
        );
        for word in list {
            assert!(!word.is_empty(), "empty word");
            assert!(
                word.bytes().all(|b| b.is_ascii_lowercase()),
                "{word:?} is not lowercase ascii letters"
            );
        }
        let unique: BTreeSet<_> = list.iter().collect();
        assert_eq!(unique.len(), list.len(), "duplicate word in list");
    }

    #[test]
    fn adjectives_are_curated() {
        assert_curated(ADJECTIVES);
    }

    #[test]
    fn nouns_are_curated() {
        assert_curated(NOUNS);
    }

    #[test]
    fn lists_do_not_share_words() {
        let adjectives: BTreeSet<_> = ADJECTIVES.iter().collect();
        let shared: Vec<_> = NOUNS.iter().filter(|n| adjectives.contains(n)).collect();
        assert!(shared.is_empty(), "words in both lists: {shared:?}");
    }

    #[test]
    fn same_seed_yields_same_names() {
        let mut a = NameRng::seeded(42);
        let mut b = NameRng::seeded(42);
        for _ in 0..16 {
            assert_eq!(random_name(&mut a), random_name(&mut b));
        }
    }

    #[test]
    fn generated_name_is_one_adjective_and_one_noun() {
        let mut rng = NameRng::seeded(7);
        for _ in 0..256 {
            let name = random_name(&mut rng);
            let (adjective, noun) = name.split_once('-').expect("adjective-noun");
            assert!(ADJECTIVES.contains(&adjective), "{adjective}");
            assert!(NOUNS.contains(&noun), "{noun}");
        }
    }

    #[test]
    fn successive_picks_vary() {
        let mut rng = NameRng::seeded(1);
        let names: BTreeSet<_> = (0..32).map(|_| random_name(&mut rng)).collect();
        assert!(names.len() > 16, "picks barely vary: {names:?}");
    }

    #[test]
    fn detects_the_placeholder() {
        assert!(template_has_random_name("${random-name}"));
        assert!(template_has_random_name("work-${random-name}"));
        assert!(!template_has_random_name("${cwd-basename}"));
        assert!(!template_has_random_name("default"));
    }
}
