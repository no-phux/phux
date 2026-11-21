//! `[rune.keys]` over the command table, and `[rune] key-defaults`.

use super::{COMMANDS, Chord, Shortcut, id};

/// Chords from `[rune.keys]`; `None` takes the chord away from the table.
type Bound = Vec<(String, Option<Shortcut>)>;

/// [`Bound`], and whether the table's own chords still apply.
// ponytail: menus and the shortcut list still show the built-in chords;
// build them from this map too if people rebind enough to be misled.
static USER: std::sync::OnceLock<(Bound, bool)> = std::sync::OnceLock::new();

/// The commands whose built-in chords stay with `key-defaults = false`
/// until `[rune.keys]` binds them: the way to everything else.
const ESCAPE_HATCH: [Shortcut; 2] = [Shortcut::Palette, Shortcut::Settings];

/// A chord as the table spells it: lower case, modifiers in `cmd`, `ctrl`,
/// `alt`, `shift` order, then the key.
fn canonical(chord: &str) -> String {
    let lower = chord.to_ascii_lowercase();
    let mut parts: Vec<&str> = lower.split('+').map(str::trim).collect();
    let key = parts.pop().unwrap_or_default();
    let held = ["cmd", "ctrl", "alt", "shift"]
        .into_iter()
        .filter(|name| parts.contains(name));
    held.chain([key]).collect::<Vec<_>>().join("+")
}

/// Read `[rune.keys]`: the bindings, and a line for each one not understood.
pub fn user_keys(keys: &std::collections::BTreeMap<String, String>) -> (Bound, Vec<String>) {
    let mut problems = Vec::new();
    let mut bound = Vec::new();
    for (chord, name) in keys {
        // The names before `scroll-to-top` and `scroll-to-bottom`.
        let name = match name.as_str() {
            "scroll-top" => "scroll-to-top",
            "scroll-bottom" => "scroll-to-bottom",
            name => name,
        };
        let action = COMMANDS.iter().map(|command| command.action);
        match (name, action.into_iter().find(|a| id(*a) == name)) {
            ("none", _) => bound.push((canonical(chord), None)),
            (_, Some(action)) => bound.push((canonical(chord), Some(action))),
            (_, None) => problems.push(format!("{chord}: no command named {name}")),
        }
    }
    (bound, problems)
}

/// Install `[rune.keys]` for this run, over the built-in chords unless
/// `defaults` is false, and return what was not understood.
pub fn bind(keys: &std::collections::BTreeMap<String, String>, defaults: bool) -> Vec<String> {
    let (bound, problems) = user_keys(keys);
    let _ = USER.set((bound, defaults));
    problems
}

/// What `chord` does: a `[rune.keys]` binding, else the table's chord when
/// `defaults` holds or it reaches an unbound [`ESCAPE_HATCH`] command.
pub(super) fn lookup(
    user: &[(String, Option<Shortcut>)],
    defaults: bool,
    chord: Chord,
) -> Option<Shortcut> {
    let spelled = chord.accelerator();
    if let Some((_, action)) = user.iter().find(|(key, _)| *key == spelled) {
        return *action;
    }
    let kept = |action: Shortcut| {
        defaults || (ESCAPE_HATCH.contains(&action) && user.iter().all(|(_, a)| *a != Some(action)))
    };
    COMMANDS
        .iter()
        .find(|command| command.chords().contains(&chord))
        .map(|command| command.action)
        .filter(|action| kept(*action))
}

/// What a chord does here, if anything.
pub fn action_of(chord: Chord) -> Option<Shortcut> {
    let (user, defaults) = USER
        .get()
        .map_or((&[][..], true), |(u, d)| (u.as_slice(), *d));
    lookup(user, defaults, chord)
}
