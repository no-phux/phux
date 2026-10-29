//! The legend's model: which-key rows built from a [`KeybindingsCfg`],
//! grouped, ordered, and labelled in the palette's words.

use phux_config::{Action, KeybindingsCfg};

/// The sections the legend groups bindings under, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Group {
    Pane,
    Window,
    Session,
    Phux,
}

impl Group {
    pub(super) const fn header(self) -> &'static str {
        match self {
            Self::Pane => "Pane",
            Self::Window => "Tab",
            Self::Session => "Session",
            Self::Phux => "phux",
        }
    }

    /// The section an action belongs to, from its name: anything that is
    /// not about a pane, a window, or a session is phux's own chrome.
    pub(super) fn of(action: &str) -> Self {
        const PANE: &[&str] = &[
            "copy-mode",
            "find-path",
            "insert-path",
            "take-input",
            "give-input",
            "signal-terminal",
            "toggle-zoom",
            "focus-direction",
        ];
        if action.ends_with("-pane") || PANE.contains(&action) {
            Self::Pane
        } else if action.contains("window") || action == "go-to-directory" {
            Self::Window
        } else if action.contains("session") || action == "detach" {
            Self::Session
        } else {
            Self::Phux
        }
    }
}

/// Reading order inside a group; unlisted actions follow, by label.
pub(super) const ACTION_ORDER: &[&str] = &[
    "split-pane",
    "kill-pane",
    "focus-direction",
    "resize-pane",
    "next-pane",
    "previous-pane",
    "last-pane",
    "toggle-zoom",
    "move-pane",
    "copy-mode",
    "find-path",
    "take-input",
    "give-input",
    "signal-terminal",
    "new-window",
    "kill-window",
    "select-window",
    "next-window",
    "previous-window",
    "window-picker",
    "rename-window",
    "move-window",
    "go-to-directory",
    "new-session",
    "session-picker",
    "next-session",
    "previous-session",
    "last-session",
    "rename-session",
    "detach",
    "command-palette",
    "show-help",
    "agent-fleet",
    "next-attention",
    "return-from-attention",
    "toggle-sidebar",
    "settings",
    "reload-config",
    "report-bug",
];

/// Actions whose bindings differ only by `direction` and read best as one
/// row (`h j k l  focus`), with that row's label.
pub(super) const DIRECTIONAL: &[(&str, &str)] =
    &[("focus-direction", "Focus"), ("resize-pane", "Resize")];

/// Cardinal directions in the order a directional row lists its keys.
pub(super) const DIRECTIONS: [&str; 4] = ["left", "down", "up", "right"];

/// One legend row: the keys that reach it and what they do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Row {
    pub(super) group: Group,
    /// The action the keys dispatch.
    pub(super) action: String,
    /// Sort key inside the group (see [`ACTION_ORDER`]).
    pub(super) rank: usize,
    pub(super) keys: Vec<String>,
    pub(super) label: String,
}

impl Row {
    pub(super) fn key_text(&self) -> String {
        self.keys.join(" ")
    }
}

/// One legend row as plain data, for the generated key reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegendRow {
    /// The section header the row is listed under (`Pane`, `Tab`, ...).
    pub group: &'static str,
    /// The action the keys dispatch.
    pub action: String,
    /// The prefix-table keys that reach it (`0-9` for the tab jumps).
    pub keys: Vec<String>,
    /// What the keys do, in the palette's words.
    pub label: String,
}

/// The rows the popup shows for `cfg`, in display order.
#[must_use]
pub fn legend(cfg: &KeybindingsCfg) -> Vec<LegendRow> {
    rows_for(cfg)
        .into_iter()
        .map(|row| LegendRow {
            group: row.group.header(),
            action: row.action,
            keys: row.keys,
            label: row.label,
        })
        .collect()
}

/// Legend rows in display order: by group, then [`ACTION_ORDER`], then label.
pub(super) fn rows_for(cfg: &KeybindingsCfg) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    let mut window_jump_keys: Vec<String> = Vec::new();
    // (family, direction, key) for every directional binding.
    let mut directional: Vec<(usize, usize, &str)> = Vec::new();
    for (key, action) in &cfg.prefix_table {
        if is_indexed_select_window(action) {
            window_jump_keys.push(key.clone());
        } else if let Some((family, direction)) = directional_family(action) {
            directional.push((family, direction, key));
        } else {
            push_merged(
                &mut rows,
                action_name(action),
                key.clone(),
                action_label(action),
            );
        }
    }
    if let Some((key, label)) = compact_window_jump_keys(&window_jump_keys) {
        push_merged(&mut rows, "select-window", key, label);
    }
    push_directional(&mut rows, &mut directional);
    rows.sort_by(|a, b| (a.group, a.rank, &a.label).cmp(&(b.group, b.rank, &b.label)));
    rows
}

pub(super) fn action_name(action: &Action) -> &str {
    match action {
        Action::Bare(name) => name,
        Action::Parameterized(p) => &p.action,
    }
}

pub(super) fn rank_of(name: &str) -> usize {
    ACTION_ORDER
        .iter()
        .position(|n| *n == name)
        .unwrap_or(ACTION_ORDER.len())
}

/// Add `key` to the row already carrying `label`, or start one: two keys
/// bound to the same thing are one row, not two.
pub(super) fn push_merged(rows: &mut Vec<Row>, name: &str, key: String, label: String) {
    if let Some(row) = rows.iter_mut().find(|r| r.label == label) {
        row.keys.push(key);
        return;
    }
    rows.push(Row {
        group: Group::of(name),
        action: name.to_owned(),
        rank: rank_of(name),
        keys: vec![key],
        label,
    });
}

/// `(index into DIRECTIONAL, index into DIRECTIONS)` for a binding that
/// joins a directional row: the action is in [`DIRECTIONAL`] and carries a
/// cardinal `direction`.
pub(super) fn directional_family(action: &Action) -> Option<(usize, usize)> {
    let Action::Parameterized(p) = action else {
        return None;
    };
    let family = DIRECTIONAL.iter().position(|(name, _)| *name == p.action)?;
    let direction = p.args.get("direction")?.as_str()?;
    let order = DIRECTIONS.iter().position(|d| *d == direction)?;
    Some((family, order))
}

/// One row per directional family, keys in left/down/up/right order.
pub(super) fn push_directional(rows: &mut Vec<Row>, bindings: &mut [(usize, usize, &str)]) {
    bindings.sort_unstable();
    for (family, (name, label)) in DIRECTIONAL.iter().enumerate() {
        let keys: Vec<String> = bindings
            .iter()
            .filter(|(f, _, _)| *f == family)
            .map(|(_, _, key)| (*key).to_owned())
            .collect();
        if keys.is_empty() {
            continue;
        }
        rows.push(Row {
            group: Group::of(name),
            action: (*name).to_owned(),
            rank: rank_of(name),
            keys,
            label: (*label).to_owned(),
        });
    }
}

/// A binding's label as a person reads it: the palette title of the
/// [`ACTIONS`](crate::attach::action_registry::ACTIONS) palette row the binding
/// commits exactly, else a phrase built from the action name and its
/// argument values, so new and plugin actions are never unlabelled.
pub(super) fn action_label(action: &Action) -> String {
    let resolved = phux_config::keybind::ResolvedAction::from(action);
    if let Some((title, _)) = crate::attach::action_registry::ACTIONS
        .iter()
        .filter(|spec| spec.resolved_action() == resolved)
        .find_map(crate::attach::action_registry::ActionSpec::palette_row)
    {
        return title.to_owned();
    }
    let (name, args) = (resolved.action.as_str(), &resolved.args);
    let arg = |key: &str| {
        args.get(key)
            .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned))
    };
    match (name, arg("direction").as_deref(), arg("delta").as_deref()) {
        ("split-pane", Some("horizontal"), _) => return "Split below".to_owned(),
        ("move-window", _, Some("-1")) => return "Move tab left".to_owned(),
        ("move-window", _, Some("1")) => return "Move tab right".to_owned(),
        ("resize-pane", Some(direction), _) => return format!("Resize {direction}"),
        ("focus-direction", Some(direction), _) => return format!("Focus {direction}"),
        _ => {}
    }
    if let ("signal-terminal", Some(signal)) = (name, arg("signal")) {
        return sentence_case(&format!("{signal} pane"));
    }
    if let ("enter-mode", Some(mode)) = (name, arg("name")) {
        return sentence_case(&format!("{mode} mode"));
    }
    let mut label = name.replace('-', " ").replace("window", "tab");
    for value in args.values() {
        label.push(' ');
        label.push_str(
            &value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_owned),
        );
    }
    sentence_case(&label)
}

/// `text` with its first letter upper-cased, the palette's title case.
pub(super) fn sentence_case(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

pub(super) fn is_indexed_select_window(action: &Action) -> bool {
    matches!(
        action,
        Action::Parameterized(p) if p.action == "select-window" && p.args.contains_key("index")
    )
}

/// Collapse sorted numeric window-jump keys into one `0-9` row (a lone key
/// when only one is bound). Modified keys join with `..` (`M-1..M-9`), where
/// a `-` would read as part of the key.
pub(super) fn compact_window_jump_keys(keys: &[String]) -> Option<(String, String)> {
    let first = keys.first()?;
    let last = keys.last()?;
    let key = match (keys.len(), first.chars().count()) {
        (1, _) => first.clone(),
        (_, 1) => format!("{first}-{last}"),
        _ => format!("{first}..{last}"),
    };
    Some((key, "Go to tab".to_owned()))
}
