//! Your own keys for copy mode. The key mode named `copy-mode`
//! (`[keybindings.modes.copy-mode]`) maps a chord to one of the built-in
//! keys with `copy-mode-key`, so a press of yours is read as the key the
//! overlay already understands, or to `clear-selection`.

use std::collections::BTreeMap;
use std::sync::RwLock;

use phux_config::Action;
use phux_config::keybind::{KeyChord, ResolvedAction, parse_chord};
use phux_protocol::input::key::KeyEvent;

/// The key mode this reads, and the action its entries use.
const MODE: &str = "copy-mode";
const ACTION: &str = "copy-mode-key";
const CLEAR: &str = "clear-selection";

/// What a chord of yours stands for: a built-in chord and the character it
/// types when it is one, or `None` for `clear-selection`.
type Remap = Vec<(KeyChord, Option<(KeyChord, Option<String>)>)>;

/// A press of yours, as copy mode reads it.
pub(super) enum Yours {
    /// The built-in key it stands for.
    Key(KeyEvent),
    /// Drop the selection.
    ClearSelection,
}

// ponytail: one table for the process, because the overlay is built deep in
// the dispatcher with no config in reach; pass it in if two clients ever
// share a process.
static KEYS: RwLock<Remap> = RwLock::new(Vec::new());

/// Read the `copy-mode` key mode; returns the entries that were not understood.
pub fn set_copy_mode_keys(modes: &BTreeMap<String, BTreeMap<String, Action>>) -> Vec<String> {
    let (remap, problems) = read(modes.get(MODE));
    if let Ok(mut keys) = KEYS.write() {
        *keys = remap;
    }
    problems
}

fn read(table: Option<&BTreeMap<String, Action>>) -> (Remap, Vec<String>) {
    let mut remap = Vec::new();
    let mut problems = Vec::new();
    for (chord, action) in table.into_iter().flatten() {
        let resolved = ResolvedAction::from(action);
        if resolved.action == CLEAR {
            match parse_chord(chord) {
                Ok(from) => remap.push((from, None)),
                Err(_) => problems.push(format!("{MODE}: {chord} is not a chord")),
            }
            continue;
        }
        // Any other action in this table is someone's own key mode.
        if resolved.action != ACTION {
            continue;
        }
        let target = resolved.args.get("key").and_then(toml::Value::as_str);
        match (parse_chord(chord), target.map(parse_chord)) {
            (Ok(from), Some(Ok(to))) => {
                let text = target.filter(|key| key.chars().count() == 1);
                remap.push((from, Some((to, text.map(str::to_owned)))));
            }
            _ => problems.push(format!("{MODE}: {chord} needs a `key` that is a chord")),
        }
    }
    (remap, problems)
}

/// What `event` stands for, when it is one of yours.
pub(super) fn remap(event: &KeyEvent) -> Option<Yours> {
    let keys = KEYS.read().ok()?;
    translate(&keys, event)
}

fn translate(keys: &Remap, event: &KeyEvent) -> Option<Yours> {
    let pressed = KeyChord {
        modifiers: event.mods,
        key: event.key,
    };
    let (_, to) = keys.iter().find(|(from, _)| *from == pressed)?;
    let Some((to, text)) = to else {
        return Some(Yours::ClearSelection);
    };
    Some(Yours::Key(KeyEvent {
        key: to.key,
        mods: to.modifiers,
        text: text.clone(),
        ..event.clone()
    }))
}

#[cfg(test)]
mod tests {
    use phux_protocol::input::key::{ModSet, PhysicalKey};

    use super::*;

    fn key_action(key: &str) -> Action {
        let mut args = BTreeMap::new();
        args.insert("key".to_owned(), toml::Value::String(key.to_owned()));
        Action::Parameterized(phux_config::ParamAction {
            action: ACTION.to_owned(),
            args,
        })
    }

    #[test]
    fn a_chord_of_yours_reads_as_the_built_in_key_and_a_bad_entry_is_reported() {
        let table: BTreeMap<String, Action> = [
            ("C-n".to_owned(), key_action("j")),
            ("M-v".to_owned(), key_action("C-b")),
            ("C-x".to_owned(), key_action("not a key at all")),
            ("C-q".to_owned(), Action::Bare("detach".to_owned())),
            ("C-g".to_owned(), Action::Bare(CLEAR.to_owned())),
        ]
        .into();
        let (keys, problems) = read(Some(&table));
        assert_eq!(problems, ["copy-mode: C-x needs a `key` that is a chord"]);

        let press = |key, mods| crate::test_util::press_mods(key, mods);
        let built_in = |key, mods| match translate(&keys, &press(key, mods)) {
            Some(Yours::Key(event)) => event,
            _ => panic!("{key:?} stands for a built-in key"),
        };
        let down = built_in(PhysicalKey::N, ModSet::CTRL);
        assert_eq!((down.key, down.mods), (PhysicalKey::J, ModSet::empty()));
        assert_eq!(down.text.as_deref(), Some("j"));
        let page = built_in(PhysicalKey::V, ModSet::ALT);
        assert!(matches!(
            translate(&keys, &press(PhysicalKey::G, ModSet::CTRL)),
            Some(Yours::ClearSelection)
        ));
        assert_eq!(
            (page.key, page.mods, page.text),
            (PhysicalKey::B, ModSet::CTRL, None)
        );
        assert!(translate(&keys, &press(PhysicalKey::Q, ModSet::CTRL)).is_none());
        assert!(translate(&keys, &press(PhysicalKey::J, ModSet::empty())).is_none());
    }
}
