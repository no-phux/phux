//! The file tree's rows: changed files under their folders, folders first,
//! with a chain of single-folder parents folded into one row (`src/app`), as
//! Zed and VS Code show it. Pure, so it tests without a window.

use std::collections::{BTreeMap, HashSet};

use crate::git::Entry;

/// One painted row of a section's tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Row<'a> {
    Folder {
        /// `scope:path`, the key [`rows`] checks against `collapsed`.
        key: String,
        /// The folder, or the folded chain of folders, relative to its parent.
        label: String,
        depth: usize,
        open: bool,
        /// Files anywhere beneath it.
        files: usize,
    },
    File {
        entry: &'a Entry,
        depth: usize,
    },
}

#[derive(Default)]
struct Node<'a> {
    folders: BTreeMap<&'a str, Node<'a>>,
    files: Vec<&'a Entry>,
}

impl<'a> Node<'a> {
    fn insert(&mut self, entry: &'a Entry) {
        let mut node = self;
        let mut parts = entry.path.split('/').peekable();
        while let Some(part) = parts.next() {
            if parts.peek().is_none() {
                node.files.push(entry);
            } else {
                node = node.folders.entry(part).or_default();
            }
        }
    }

    fn count(&self) -> usize {
        self.files.len() + self.folders.values().map(Node::count).sum::<usize>()
    }
}

/// `entries` as tree rows. `scope` keeps one section's folder keys apart from
/// another's, so collapsing `src` under Staged leaves it open under Changes.
pub(crate) fn rows<'a>(
    entries: &[&'a Entry],
    scope: &str,
    collapsed: &HashSet<String>,
) -> Vec<Row<'a>> {
    let mut root = Node::default();
    for entry in entries {
        root.insert(entry);
    }
    let mut out = Vec::new();
    walk(&root, "", 0, scope, collapsed, &mut out);
    out
}

fn walk<'a>(
    node: &Node<'a>,
    prefix: &str,
    depth: usize,
    scope: &str,
    collapsed: &HashSet<String>,
    out: &mut Vec<Row<'a>>,
) {
    for (name, child) in &node.folders {
        let (mut label, mut path, mut folder) = ((*name).to_owned(), join(prefix, name), child);
        while folder.files.is_empty() && folder.folders.len() == 1 {
            let (name, only) = folder.folders.iter().next().expect("one folder");
            label = format!("{label}/{name}");
            path = join(&path, name);
            folder = only;
        }
        let key = format!("{scope}:{path}");
        let open = !collapsed.contains(&key);
        out.push(Row::Folder {
            key,
            label,
            depth,
            open,
            files: folder.count(),
        });
        if open {
            walk(folder, &path, depth + 1, scope, collapsed, out);
        }
    }
    let mut files = node.files.clone();
    files.sort_by(|a, b| a.split_name().0.cmp(b.split_name().0));
    out.extend(files.into_iter().map(|entry| Row::File { entry, depth }));
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}/{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::Area;

    fn entry(path: &str) -> Entry {
        Entry {
            area: Area::Unstaged,
            path: path.into(),
            from: None,
            code: 'M',
        }
    }

    fn labels(rows: &[Row<'_>]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                Row::Folder {
                    label, depth, open, ..
                } => format!(
                    "{}{label}/{}",
                    "  ".repeat(*depth),
                    if *open { "" } else { "+" }
                ),
                Row::File { entry, depth } => {
                    format!("{}{}", "  ".repeat(*depth), entry.split_name().0)
                }
            })
            .collect()
    }

    #[test]
    fn folders_come_first_and_single_folder_chains_fold() {
        let entries = [
            entry("README.md"),
            entry("clients/desktop/native/src/git.rs"),
            entry("clients/desktop/native/src/app/view.rs"),
            entry("clients/desktop/native/Cargo.toml"),
            entry("docs/a.md"),
        ];
        let refs: Vec<&Entry> = entries.iter().collect();
        let rows = rows(&refs, "changes", &HashSet::new());
        assert_eq!(
            labels(&rows),
            [
                "clients/desktop/native/",
                "  src/",
                "    app/",
                "      view.rs",
                "    git.rs",
                "  Cargo.toml",
                "docs/",
                "  a.md",
                "README.md",
            ]
        );
        assert!(matches!(rows[0], Row::Folder { files: 3, .. }));
    }

    #[test]
    fn a_collapsed_folder_hides_what_it_holds_in_its_scope_only() {
        let entries = [entry("src/a.rs"), entry("src/b.rs"), entry("top.rs")];
        let refs: Vec<&Entry> = entries.iter().collect();
        let collapsed = HashSet::from(["changes:src".to_owned()]);
        assert_eq!(
            labels(&rows(&refs, "changes", &collapsed)),
            ["src/+", "top.rs"]
        );
        assert_eq!(rows(&refs, "staged", &collapsed).len(), 4);
    }
}
