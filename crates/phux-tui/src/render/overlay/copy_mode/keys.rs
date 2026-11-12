//! Your own keys for copy mode. The key mode named `copy-mode`
//! (`[keybindings.modes.copy-mode]`) maps a chord to one of the built-in
//! keys with `copy-mode-key`, so a press of yours is read as the key the
//! overlay already understands.

use std::collections::BTreeMap;
use std::sync::RwLock;

use phux_config::Action;
use phux_config::keybind::{KeyChord, ResolvedAction, parse_chord};
use phux_protocol::input::key::KeyEvent;

/// The key mode this reads, and the action its entries use.
const MODE: &str = "copy-mode";
const ACTION: &str = "copy-mode-key";

/// What a chord of yours stands for: a built-in chord, and the character it
/// types when it is one.
type Remap = Vec<(KeyChord, KeyChord, Option<String>)>;

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
        // Any other action in this table is someone's own key mode.
        if resolved.action != ACTION {
            continue;
        }
        let target = resolved.args.get("key").and_then(toml::Value::as_str);
        match (parse_chord(chord), target.map(parse_chord)) {
            (Ok(from), Some(Ok(to))) => {
                let text = target.filter(|key| key.chars().count() == 1);
                remap.push((from, to, text.map(str::to_owned)));
            }
            _ => problems.push(format!("{MODE}: {chord} needs a `key` that is a chord")),
        }
    }
    (remap, problems)
}

/// `event` as the built-in key it stands for, when it is one of yours.
pub(super) fn remap(event: &KeyEvent) -> Option<KeyEvent> {
    let keys = KEYS.read().ok()?;
    translate(&keys, event)
}

fn translate(keys: &Remap, event: &KeyEvent) -> Option<KeyEvent> {
    let pressed = KeyChord {
        modifiers: event.mods,
        key: event.key,
    };
    let (_, to, text) = keys.iter().find(|(from, _, _)| *from == pressed)?;
    Some(KeyEvent {
        key: to.key,
        mods: to.modifiers,
        text: text.clone(),
        ..event.clone()
    })
}

#[cfg(test)]
mod tests {
    use phux_protocol::input::key::{KeyAction, ModSet, PhysicalKey};

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
        ]
        .into();
        let (keys, problems) = read(Some(&table));
        assert_eq!(problems, ["copy-mode: C-x needs a `key` that is a chord"]);

        let press = |key, mods| KeyEvent {
            action: KeyAction::Press,
            key,
            mods,
            consumed_mods: ModSet::empty(),
            composing: false,
            text: None,
            unshifted_codepoint: None,
        };
        let down = translate(&keys, &press(PhysicalKey::N, ModSet::CTRL)).expect("C-n");
        assert_eq!((down.key, down.mods), (PhysicalKey::J, ModSet::empty()));
        assert_eq!(down.text.as_deref(), Some("j"));
        let page = translate(&keys, &press(PhysicalKey::V, ModSet::ALT)).expect("M-v");
        assert_eq!(
            (page.key, page.mods, page.text),
            (PhysicalKey::B, ModSet::CTRL, None)
        );
        assert!(translate(&keys, &press(PhysicalKey::Q, ModSet::CTRL)).is_none());
        assert!(translate(&keys, &press(PhysicalKey::J, ModSet::empty())).is_none());
    }
}
