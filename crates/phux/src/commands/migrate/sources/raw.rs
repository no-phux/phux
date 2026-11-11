//! zellij and herdr: settings are reported, key lines go to the agent to
//! translate itself.

use std::path::Path;

use super::{Class, Line, Source, Verdict, read_tree, skip};
use crate::commands::MigrateSource;

pub(super) fn zellij(path: &Path) -> Source {
    let mut files = Vec::new();
    let mut rows = Vec::new();
    // Brace depth inside `keybinds { ... }`; 0 outside it.
    let mut depth = 0_usize;
    let mut decide = |_: &Path, line: &str| {
        if line.is_empty() || line.starts_with("//") {
            return Line::Nothing;
        }
        if depth == 0 && !line.starts_with("keybinds") {
            if line
                .trim_matches(|c| c == '{' || c == '}' || c == ' ')
                .is_empty()
            {
                return Line::Nothing;
            }
            return skip(
                Class::Setting,
                "a zellij setting; this migration moves key bindings only",
            );
        }
        // ponytail: counts braces without parsing KDL strings; a `{` inside
        // a quoted key would end the block early.
        depth = (depth + line.matches('{').count()).saturating_sub(line.matches('}').count());
        let class = if line.starts_with("unbind") {
            Class::Unbinding
        } else if line.starts_with("bind ") {
            Class::Binding
        } else {
            Class::Other
        };
        Line::Row(class, Verdict::Raw, None)
    };
    read_tree(path, &mut files, &mut rows, &mut decide);
    Source {
        kind: MigrateSource::Zellij,
        files,
        rows,
        note: None,
    }
}

pub(super) fn herdr(path: &Path) -> Source {
    let mut files = Vec::new();
    let mut rows = Vec::new();
    let mut keys = false;
    let mut decide = |_: &Path, line: &str| {
        if line.is_empty() || line.starts_with('#') {
            return Line::Nothing;
        }
        if line.starts_with('[') {
            keys = line
                .trim_matches(|c| c == '[' || c == ']')
                .starts_with("keys");
            return if keys {
                Line::Row(Class::Other, Verdict::Raw, None)
            } else {
                Line::Nothing
            };
        }
        if keys {
            return Line::Row(Class::Binding, Verdict::Raw, None);
        }
        skip(
            Class::Setting,
            "a herdr setting; this migration moves key bindings only",
        )
    };
    read_tree(path, &mut files, &mut rows, &mut decide);
    Source {
        kind: MigrateSource::Herdr,
        files,
        rows,
        note: None,
    }
}
