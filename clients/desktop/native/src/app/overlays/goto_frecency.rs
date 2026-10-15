//! How often and how recently each Go To destination was opened.
//!
//! A typed query adds a capped boost so a familiar row rises among matches.
//! An empty query keeps section order. The boost never creates a match.
//! History stays in this window. A key is the daemon boot plus a pane or tab
//! id, and both fields are length-capped, so the store is not a count of
//! unbounded strings. Only a navigation that still finds its destination
//! records a visit. Chips, queries, highlights, and other palette actions do
//! not.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// Destinations remembered. Past this, the least frecent key is dropped.
pub(super) const MAX_KEYS: usize = 256;
/// Bytes in one key field. A longer or empty field is not a key.
pub(super) const MAX_FIELD_BYTES: usize = 256;
/// Retained key bytes: every field of every kept key, at the field cap.
pub(super) const MAX_RETAINED_BYTES: usize = MAX_KEYS * (2 * MAX_FIELD_BYTES);
/// A choice counts half as much after this long.
const HALF_LIFE_SECS: f64 = 7.0 * 24.0 * 60.0 * 60.0;
/// Turns a decayed visit count into ranking points. One recent visit is a
/// small tie-break; the boost grows logarithmically.
const BOOST_SCALE: f64 = 16.0;
/// A familiar row can outrank a slightly better match, never a far better one.
pub(super) const MAX_BOOST: i64 = 64;

/// A destination this window remembers opening, scoped to one daemon boot.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum Key {
    Pane { boot: String, id: String },
    Tab { boot: String, id: String },
}

impl Key {
    pub(super) fn pane(boot: &str, id: &str) -> Option<Self> {
        checked(boot, id).map(|(boot, id)| Self::Pane { boot, id })
    }

    pub(super) fn tab(boot: &str, id: &str) -> Option<Self> {
        checked(boot, id).map(|(boot, id)| Self::Tab { boot, id })
    }

    fn bytes(&self) -> usize {
        match self {
            Self::Pane { boot, id } | Self::Tab { boot, id } => boot.len() + id.len(),
        }
    }
}

fn fits(text: &str) -> bool {
    !text.is_empty() && text.len() <= MAX_FIELD_BYTES
}

fn checked(boot: &str, id: &str) -> Option<(String, String)> {
    if fits(boot) && fits(id) {
        Some((boot.to_owned(), id.to_owned()))
    } else {
        None
    }
}

/// A confirmed palette row. `listed` is whether that pane or tab still exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Choice<'a> {
    Pane {
        id: &'a str,
        listed: bool,
    },
    Tab {
        id: &'a str,
        listed: bool,
    },
    /// Filter, query, highlight, command, session, path, or move.
    Other,
}

/// The key to remember, or nothing when the row was not an open of a listed
/// pane or tab on a known boot.
pub(super) fn validated_key(boot: Option<&str>, choice: Choice<'_>) -> Option<Key> {
    let boot = boot?;
    match choice {
        Choice::Pane { id, listed: true } => Key::pane(boot, id),
        Choice::Tab { id, listed: true } => Key::tab(boot, id),
        Choice::Pane { listed: false, .. } | Choice::Tab { listed: false, .. } | Choice::Other => {
            None
        }
    }
}

#[derive(Clone, Debug)]
struct Visit {
    key: Key,
    /// Decayed visit count as of `at`.
    score: f64,
    /// Unix seconds of the latest visit.
    at: u64,
}

impl Visit {
    /// The score decayed to `now`. A stamp in the future counts as now.
    fn score_at(&self, now: u64) -> f64 {
        let elapsed = now.saturating_sub(self.at) as f64;
        self.score * 0.5_f64.powf(elapsed / HALF_LIFE_SECS)
    }
}

/// Unix seconds; a clock before 1970 reads as the epoch.
pub(super) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// Go To history for one window.
#[derive(Clone, Debug, Default)]
pub(crate) struct Store {
    visits: Vec<Visit>,
    generation: u64,
}

impl Store {
    pub(super) fn generation(&self) -> u64 {
        self.generation
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.visits.len()
    }

    fn retained_bytes(&self) -> usize {
        self.visits.iter().map(|visit| visit.key.bytes()).sum()
    }

    /// Ranking points for `key` at `now`, from zero to [`MAX_BOOST`].
    #[cfg(test)]
    fn boost(&self, key: &Key, now: u64) -> i64 {
        self.visits
            .iter()
            .find(|visit| &visit.key == key)
            .map(|visit| points(visit.score_at(now)))
            .unwrap_or(0)
    }

    /// Points for every remembered key at `now`.
    pub(super) fn boosts(&self, now: u64) -> HashMap<Key, i64> {
        self.visits
            .iter()
            .map(|visit| (visit.key.clone(), points(visit.score_at(now))))
            .collect()
    }

    /// Record `key` when a navigation was validated. `None` changes nothing:
    /// a chip, a query, a highlight, or a missing destination.
    pub(super) fn note(&mut self, key: Option<Key>, now: u64) {
        let Some(key) = key else {
            return;
        };
        self.visit(key, now);
    }

    fn visit(&mut self, key: Key, now: u64) {
        if !fits_key(&key) {
            return;
        }
        match self.visits.iter_mut().find(|visit| visit.key == key) {
            Some(visit) => {
                visit.score = visit.score_at(now) + 1.0;
                visit.at = visit.at.max(now);
            }
            None => self.visits.push(Visit {
                key,
                score: 1.0,
                at: now,
            }),
        }
        self.trim(now);
        self.generation = self.generation.wrapping_add(1);
    }

    /// Drop the least frecent key until both the count and the byte budget hold.
    fn trim(&mut self, now: u64) {
        while self.visits.len() > MAX_KEYS || self.retained_bytes() > MAX_RETAINED_BYTES {
            let Some(weakest) = self
                .visits
                .iter()
                .enumerate()
                .min_by(|(_, left), (_, right)| left.score_at(now).total_cmp(&right.score_at(now)))
                .map(|(index, _)| index)
            else {
                break;
            };
            self.visits.swap_remove(weakest);
        }
    }
}

fn fits_key(key: &Key) -> bool {
    match key {
        Key::Pane { boot, id } | Key::Tab { boot, id } => fits(boot) && fits(id),
    }
}

fn points(score: f64) -> i64 {
    let boosted = (score.ln_1p() * BOOST_SCALE).round();
    boosted.clamp(0.0, MAX_BOOST as f64) as i64
}

#[cfg(test)]
mod tests {
    use super::{
        Choice, Key, MAX_BOOST, MAX_FIELD_BYTES, MAX_KEYS, MAX_RETAINED_BYTES, Store, validated_key,
    };

    const DAY: u64 = 24 * 60 * 60;

    fn pane(id: &str) -> Key {
        Key::pane("boot", id).unwrap()
    }

    #[test]
    fn validated_navigation_bumps_and_other_actions_do_not() {
        let mut store = Store::default();
        let now = 1_000;
        let opened = pane("@1");
        let ignored = [
            validated_key(Some("boot"), Choice::Other),
            validated_key(
                Some("boot"),
                Choice::Pane {
                    id: "@1",
                    listed: false,
                },
            ),
            validated_key(
                Some("boot"),
                Choice::Tab {
                    id: "layout",
                    listed: false,
                },
            ),
            validated_key(
                None,
                Choice::Pane {
                    id: "@1",
                    listed: true,
                },
            ),
            validated_key(
                Some(""),
                Choice::Pane {
                    id: "@1",
                    listed: true,
                },
            ),
        ];
        for key in ignored {
            store.note(key, now);
        }
        assert_eq!(store.generation(), 0);
        assert_eq!(store.boost(&opened, now), 0);
        assert_eq!(store.len(), 0);

        store.note(
            validated_key(
                Some("boot"),
                Choice::Pane {
                    id: "@1",
                    listed: true,
                },
            ),
            now,
        );
        let once = store.boost(&opened, now);
        assert!(once > 0);
        assert_eq!(store.generation(), 1);
        store.note(validated_key(Some("boot"), Choice::Other), now);
        assert_eq!(store.boost(&opened, now), once);
        assert_eq!(store.generation(), 1);

        let tab = Key::tab("boot", "layout").unwrap();
        store.note(
            validated_key(
                Some("boot"),
                Choice::Tab {
                    id: "layout",
                    listed: true,
                },
            ),
            now,
        );
        assert!(store.boost(&tab, now) > 0);
        // The same spelling on another boot, or the other kind, is a different key.
        assert_eq!(store.boost(&Key::pane("other", "@1").unwrap(), now), 0);
        assert_eq!(store.boost(&Key::tab("boot", "@1").unwrap(), now), 0);

        for _ in 0..10_000 {
            store.note(Some(opened.clone()), now);
        }
        assert_eq!(store.boost(&opened, now), MAX_BOOST);
    }

    #[test]
    fn the_least_frecent_key_is_dropped_not_the_oldest_or_the_least_visited() {
        let mut store = Store::default();
        let keeper = pane("@keeper");
        for _ in 0..8 {
            store.note(Some(keeper.clone()), 0);
        }
        for index in 0..(MAX_KEYS - 1) {
            store.note(Some(pane(&format!("@w{index}"))), 0);
        }
        assert_eq!(store.len(), MAX_KEYS);
        // One day later the oldest key is still the most frecent, so the
        // dropped key is a newer single visit, not the first one inserted.
        store.note(Some(pane("@new")), DAY);
        assert_eq!(store.len(), MAX_KEYS);
        assert!(store.visits.iter().any(|visit| visit.key == keeper));
        assert!(store.visits.iter().any(|visit| visit.key == pane("@new")));
        assert!(store.visits.iter().all(|visit| visit.key != pane("@w0")));

        let mut store = Store::default();
        let habit = pane("@habit");
        for _ in 0..8 {
            store.note(Some(habit.clone()), 0);
        }
        for index in 0..MAX_KEYS {
            store.note(Some(pane(&format!("@{index}"))), 400 * DAY);
        }
        assert_eq!(store.len(), MAX_KEYS);
        // Eight visits a year ago decay below one visit today.
        assert!(store.visits.iter().all(|visit| visit.key != habit));
        assert!(store.visits.iter().any(|visit| visit.key == pane("@0")));
    }

    #[test]
    fn retained_key_bytes_stay_inside_the_field_budget() {
        assert!(Key::pane("", "@1").is_none());
        assert!(Key::pane("boot", "").is_none());
        assert!(Key::tab("boot", &"t".repeat(MAX_FIELD_BYTES + 1)).is_none());
        assert!(Key::pane(&"b".repeat(MAX_FIELD_BYTES + 1), "@1").is_none());

        let boot = "b".repeat(MAX_FIELD_BYTES);
        let mut store = Store::default();
        for index in 0..MAX_KEYS {
            let mut id = index.to_string();
            id.push_str(&"i".repeat(MAX_FIELD_BYTES - id.len()));
            store.note(Key::tab(&boot, &id), 1);
        }
        assert_eq!(store.len(), MAX_KEYS);
        assert_eq!(store.retained_bytes(), MAX_RETAINED_BYTES);
        let mut id = "extra".to_owned();
        id.push_str(&"i".repeat(MAX_FIELD_BYTES - id.len()));
        let extra = Key::tab(&boot, &id).unwrap();
        store.note(Some(extra.clone()), 1);
        assert_eq!(store.len(), MAX_KEYS);
        assert_eq!(store.retained_bytes(), MAX_RETAINED_BYTES);
        assert!(store.boost(&extra, 1) > 0);
        let before = store.generation();
        store.note(Key::pane("boot", &"x".repeat(MAX_FIELD_BYTES + 1)), 1);
        assert_eq!(store.generation(), before);
        assert_eq!(store.retained_bytes(), MAX_RETAINED_BYTES);
    }
}
