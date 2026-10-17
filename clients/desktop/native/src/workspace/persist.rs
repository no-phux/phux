//! Versioned local presentation snapshot.
//!
//! It remembers placement and display preferences only: never credentials,
//! live view handles, or process state. It binds to one daemon incarnation
//! (`serverId`). A damaged or future value is not rewritten.

use super::layout::{Axis, DeskTab, LayoutNode, find_placement};
use crate::settings::{
    DisplayPrefs, default_display, display_value, parse_display, sanitize_display,
};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) mod writer;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

/// A saved leaf or split. Leaf ids are optional for older snapshots.
#[derive(Debug, Clone, PartialEq)]
pub enum SavedNode {
    /// One terminal.
    Leaf {
        /// Placement id, when the snapshot recorded one.
        id: Option<String>,
        /// Terminal spelling, not a live handle.
        terminal_id: String,
    },
    /// A binary split.
    Split {
        /// Axis.
        axis: Axis,
        /// Fraction for the first child. Must be in `(0, 1)`.
        ratio: f32,
        /// First child.
        first: Box<SavedNode>,
        /// Second child.
        second: Box<SavedNode>,
    },
}

/// One saved tab.
#[derive(Debug, Clone, PartialEq)]
pub struct SavedTab {
    /// Tab id.
    pub id: String,
    /// User title.
    pub title: Option<String>,
    /// Split tree.
    pub root: SavedNode,
    /// Focused terminal spelling.
    pub focused_terminal: String,
    /// Focused placement id, absent in older v2 snapshots.
    pub focused_id: Option<String>,
}

/// Version 2 layout. Version 1 migrates into this shape.
#[derive(Debug, Clone, PartialEq)]
pub struct SavedLayout {
    /// Always 2 after a successful parse.
    pub version: u8,
    /// Daemon incarnation, hex as the bridge stored it.
    pub server_id: String,
    /// Active tab id.
    pub active_tab: Option<String>,
    /// Tabs.
    pub tabs: Vec<SavedTab>,
    /// Display preferences.
    pub display: DisplayPrefs,
}

/// What a store read returned.
#[derive(Debug, Clone, PartialEq)]
pub enum StoreValue {
    /// The file is absent. A legacy path may still be tried by the store.
    Missing,
    /// A JSON value.
    Present(Value),
    /// Present but unreadable, or JSON that does not parse. Do not replace it.
    Damaged,
}

const MAX_DEPTH: u32 = 16;

/// Refuse automatic replacement of data this version cannot read losslessly.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedLayout {
    /// Parsed layout, when the store held version 1 or 2.
    pub initial: Option<SavedLayout>,
    /// Writes must not replace the stored bytes.
    pub blocked: bool,
}

/// Classify a raw store read.
#[must_use]
pub fn load_layout(raw: &StoreValue) -> LoadedLayout {
    let initial = match raw {
        StoreValue::Present(value) => parse_layout(value),
        StoreValue::Missing | StoreValue::Damaged => None,
    };
    LoadedLayout {
        blocked: *raw != StoreValue::Missing && initial.is_none(),
        initial,
    }
}

/// Snapshot tabs and display. View ids and credentials are omitted.
#[must_use]
pub fn save_layout(
    server_id: impl Into<String>,
    tabs: &[DeskTab],
    display: &DisplayPrefs,
    active_tab: Option<&str>,
) -> SavedLayout {
    SavedLayout {
        version: 2,
        server_id: server_id.into(),
        active_tab: active_tab.filter(|id| !id.is_empty()).map(str::to_owned),
        display: sanitize_display(display),
        tabs: tabs
            .iter()
            .map(|tab| SavedTab {
                id: tab.id.clone(),
                title: tab.title.clone().filter(|title| !title.is_empty()),
                root: save_node(&tab.root),
                focused_terminal: find_placement(&tab.root, &tab.focused_id)
                    .map(|placement| placement.terminal_label.clone())
                    .unwrap_or_default(),
                focused_id: Some(tab.focused_id.clone()),
            })
            .collect(),
    }
}

fn save_node(node: &LayoutNode) -> SavedNode {
    match node {
        LayoutNode::Leaf { placement } => SavedNode::Leaf {
            id: Some(placement.id.clone()),
            terminal_id: placement.terminal_label.clone(),
        },
        LayoutNode::Split {
            axis,
            ratio,
            first,
            second,
            ..
        } => SavedNode::Split {
            axis: *axis,
            ratio: *ratio,
            first: Box::new(save_node(first)),
            second: Box::new(save_node(second)),
        },
    }
}

/// Accept version 2, and migrate version 1's flat terminal lists into row splits.
#[must_use]
pub fn parse_layout(value: &Value) -> Option<SavedLayout> {
    let object = value.as_object()?;
    let server_id = object.get("serverId")?.as_str()?.to_owned();
    let raw_tabs = object.get("tabs")?.as_array()?;
    let version = object.get("version")?.as_u64()?;
    if !matches!(version, 1 | 2) {
        return None;
    }
    let tabs: Vec<SavedTab> = raw_tabs
        .iter()
        .filter_map(|tab| parse_tab(tab, version))
        .collect();
    if tabs.len() != raw_tabs.len() || !unique_identities(&tabs) {
        return None;
    }
    Some(SavedLayout {
        version: 2,
        server_id,
        active_tab: object
            .get("activeTab")
            .and_then(Value::as_str)
            .map(str::to_owned),
        tabs,
        display: object
            .get("display")
            .map(parse_display)
            .unwrap_or_else(default_display),
    })
}

fn unique_identities(tabs: &[SavedTab]) -> bool {
    let mut tab_ids = HashSet::new();
    let mut placement_ids = Vec::new();
    for tab in tabs {
        if !tab_ids.insert(tab.id.as_str()) {
            return false;
        }
        saved_ids(&tab.root, &mut placement_ids);
    }
    let mut seen = HashSet::new();
    placement_ids.into_iter().all(|id| seen.insert(id))
}

fn saved_ids<'a>(node: &'a SavedNode, ids: &mut Vec<&'a str>) {
    match node {
        SavedNode::Leaf { id, .. } => ids.extend(id.as_deref()),
        SavedNode::Split { first, second, .. } => {
            saved_ids(first, ids);
            saved_ids(second, ids);
        }
    }
}

fn parse_tab(value: &Value, version: u64) -> Option<SavedTab> {
    let object = value.as_object()?;
    let id = identity(object.get("id")?)?;
    let focused_terminal = object
        .get("focusedTerminal")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    if version == 1 {
        return Some(SavedTab {
            id,
            title: None,
            root: migrate_root(object.get("terminals")?.as_array()?)?,
            focused_terminal,
            focused_id: None,
        });
    }
    let focused_id = match object.get("focusedId") {
        None => None,
        Some(value) => Some(value.as_str()?.to_owned()),
    };
    Some(SavedTab {
        id,
        title: object.get("title").and_then(identity),
        root: parse_node(object.get("root")?, 0)?,
        focused_terminal,
        focused_id,
    })
}

fn parse_node(value: &Value, depth: u32) -> Option<SavedNode> {
    if depth > MAX_DEPTH {
        return None;
    }
    let object = value.as_object()?;
    match object.get("kind")?.as_str()? {
        "leaf" => Some(SavedNode::Leaf {
            terminal_id: identity(object.get("terminalId")?)?,
            id: match object.get("id") {
                None => None,
                Some(value) => Some(identity(value)?),
            },
        }),
        "split" => {
            let axis = Axis::parse(object.get("axis")?.as_str()?)?;
            let first = parse_node(object.get("first")?, depth + 1)?;
            let second = parse_node(object.get("second")?, depth + 1)?;
            let ratio = match object.get("ratio") {
                None => 0.5,
                Some(value) => value.as_f64()? as f32,
            };
            if !ratio.is_finite() || ratio <= 0.0 || ratio >= 1.0 {
                return None;
            }
            Some(SavedNode::Split {
                axis,
                ratio,
                first: Box::new(first),
                second: Box::new(second),
            })
        }
        _ => None,
    }
}

/// Version 1 tabs list terminals left to right; each split gives its first leaf an even share.
fn migrate_root(terminals: &[Value]) -> Option<SavedNode> {
    let ids: Vec<String> = terminals.iter().filter_map(identity).collect();
    if ids.len() != terminals.len() {
        return None;
    }
    ids.iter()
        .enumerate()
        .rev()
        .fold(None, |rest, (index, terminal_id)| {
            let node = SavedNode::Leaf {
                id: None,
                terminal_id: terminal_id.clone(),
            };
            Some(match rest {
                None => node,
                Some(rest) => SavedNode::Split {
                    axis: Axis::Row,
                    ratio: 1.0 / (ids.len() - index) as f32,
                    first: Box::new(node),
                    second: Box::new(rest),
                },
            })
        })
}

fn identity(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// CamelCase JSON matching the existing snapshot. Absent optional fields are omitted.
#[must_use]
pub fn layout_value(layout: &SavedLayout) -> Value {
    compact(json!({
        "version": 2,
        "serverId": layout.server_id,
        "activeTab": layout.active_tab,
        "tabs": layout.tabs.iter().map(tab_value).collect::<Vec<_>>(),
        "display": display_value(&layout.display),
    }))
}

fn tab_value(tab: &SavedTab) -> Value {
    compact(json!({
        "id": tab.id,
        "title": tab.title,
        "root": node_value(&tab.root),
        "focusedTerminal": tab.focused_terminal,
        "focusedId": tab.focused_id,
    }))
}

fn node_value(node: &SavedNode) -> Value {
    match node {
        SavedNode::Leaf { id, terminal_id } => compact(json!({
            "kind": "leaf",
            "id": id,
            "terminalId": terminal_id,
        })),
        SavedNode::Split {
            axis,
            ratio,
            first,
            second,
        } => json!({
            "kind": "split",
            "axis": axis.as_str(),
            "ratio": serde_json::Number::from_f64(f64::from(*ratio))
                .map_or(Value::from(0.5), Value::Number),
            "first": node_value(first),
            "second": node_value(second),
        }),
    }
}

/// Drop the null members an absent `Option` field serialized to.
fn compact(value: Value) -> Value {
    let Value::Object(mut object) = value else {
        return value;
    };
    object.retain(|_, member| !member.is_null());
    Value::Object(object)
}

/// Scoped layout files plus the unscoped legacy files they may import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutPaths {
    /// This socket and session's workspace.
    pub layout: PathBuf,
    /// This socket and session's quick terminal.
    pub quick: PathBuf,
    /// Unscoped workspace imported once, never overwritten.
    pub legacy_layout: PathBuf,
    /// Unscoped quick terminal imported once, never overwritten.
    pub legacy_quick: PathBuf,
}

/// `$XDG_STATE_HOME/phux-desktop/<hash>/` or `~/.local/state/phux-desktop/<hash>/`.
#[must_use]
pub fn layout_paths(socket: &str, session: &str) -> LayoutPaths {
    paths_in(PathBuf::from(target_hash(socket, session)))
}

/// Organization sessions use their immutable key, not their mutable name.
/// The separate namespace cannot collide with an existing named-session store.
#[must_use]
pub fn session_layout_paths(socket: &str, key: phux_protocol::ids::SessionKey) -> LayoutPaths {
    paths_in(Path::new("sessions").join(target_hash(socket, &key.to_string())))
}

fn paths_in(scope: PathBuf) -> LayoutPaths {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local/state")
        })
        .join("phux-desktop");
    let directory = state.join(scope);
    LayoutPaths {
        layout: directory.join("layout.json"),
        quick: directory.join("quick.json"),
        legacy_layout: state.join("layout.json"),
        legacy_quick: state.join("quick.json"),
    }
}

/// First 24 hex characters of SHA-256 over `JSON.stringify([socket, session])`.
#[must_use]
pub fn target_hash(socket: &str, session: &str) -> String {
    let payload = serde_json::to_string(&[socket, session]).unwrap_or_default();
    sha256(payload.as_bytes())
        .iter()
        .take(12)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Private layout file. Missing files fall back to `legacy` without writing it.
#[derive(Debug, Clone)]
pub struct FileLayoutStore {
    /// Current path.
    pub path: PathBuf,
    /// Imported only when `path` is absent.
    pub legacy: Option<PathBuf>,
}

impl FileLayoutStore {
    /// A store that may import `legacy` when `path` does not exist.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, legacy: Option<PathBuf>) -> Self {
        Self {
            path: path.into(),
            legacy,
        }
    }

    /// Read JSON. Absence is [`StoreValue::Missing`]. Any other failure is damaged.
    #[must_use]
    pub fn read(&self) -> StoreValue {
        match read_json(&self.path) {
            StoreValue::Missing => self
                .legacy
                .as_ref()
                .map_or(StoreValue::Missing, |path| read_json(path)),
            other => other,
        }
    }

    /// Atomically replace the file. A failure removes the staging file and leaves the target.
    pub fn write(&self, value: &Value) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let next = self.path.with_file_name(format!(
            "{}.{}.{stamp}.next",
            self.path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("layout.json"),
            std::process::id()
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&next)?;
        let result = (|| {
            file.write_all(format!("{value}\n").as_bytes())?;
            file.sync_all()?;
            drop(file);
            fs::rename(&next, &self.path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&next);
        }
        result
    }
}

fn read_json(path: &Path) -> StoreValue {
    match fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(value) => StoreValue::Present(value),
            Err(_) => StoreValue::Damaged,
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => StoreValue::Missing,
        Err(_) => StoreValue::Damaged,
    }
}

/// Local navigation history, never a server project or a recipe trust grant.
pub struct RecentProjects {
    store: FileLayoutStore,
    entries: Vec<PathBuf>,
    writer: writer::Writer,
}

impl RecentProjects {
    /// A small, socket-scoped folder history. The empty session name cannot be
    /// opened, so its layout directory is free for process-wide desktop state.
    #[must_use]
    pub fn load(socket: &str) -> Self {
        let path = layout_paths(socket, "")
            .layout
            .with_file_name("projects.json");
        Self::from_store(FileLayoutStore::new(path, None))
    }

    fn from_store(store: FileLayoutStore) -> Self {
        let entries = match store.read() {
            StoreValue::Present(value) => serde_json::from_value::<Vec<PathBuf>>(value)
                .unwrap_or_default()
                .into_iter()
                .filter(|path| path.is_absolute())
                .take(8)
                .collect(),
            StoreValue::Missing | StoreValue::Damaged => Vec::new(),
        };
        Self {
            store,
            entries,
            writer: writer::Writer::default(),
        }
    }

    #[must_use]
    pub fn entries(&self) -> &[PathBuf] {
        &self.entries
    }

    pub(crate) fn errors(&self) -> async_channel::Receiver<String> {
        self.writer.errors()
    }

    pub(crate) fn flush(&self) -> Vec<String> {
        let mut errors = self.writer.flush();
        if self.writer.failed(&self.store.path) {
            // One final attempt on close/quit also recovers an error the UI
            // listener already consumed. A persistent refusal never spins.
            match serde_json::to_value(&self.entries) {
                Ok(value) => {
                    self.writer.json(self.store.clone(), value);
                    errors.extend(self.writer.flush());
                }
                Err(error) => errors.push(error.to_string()),
            }
        }
        errors
    }

    pub fn record(&mut self, path: PathBuf) -> io::Result<()> {
        if self.entries.first() == Some(&path) {
            return self.retry_failed_save();
        }
        let mut next: Vec<_> = self
            .entries
            .iter()
            .filter(|old| **old != path)
            .take(7)
            .cloned()
            .collect();
        next.insert(0, path);
        self.save(next)
    }

    pub fn forget(&mut self, path: &Path) -> io::Result<()> {
        if !self.entries.iter().any(|old| old == path) {
            return self.retry_failed_save();
        }
        self.save(
            self.entries
                .iter()
                .filter(|old| *old != path)
                .cloned()
                .collect(),
        )
    }

    fn save(&mut self, entries: Vec<PathBuf>) -> io::Result<()> {
        let value = serde_json::to_value(&entries).map_err(io::Error::other)?;
        self.writer.json(self.store.clone(), value);
        self.entries = entries;
        Ok(())
    }

    fn retry_failed_save(&mut self) -> io::Result<()> {
        if self.writer.failed(&self.store.path) {
            self.save(self.entries.clone())?;
        }
        Ok(())
    }
}

/// Quick terminal display: the main window's preferences, sidebar hidden.
#[must_use]
pub fn quick_display(main: Option<&DisplayPrefs>, quick: Option<&SavedLayout>) -> DisplayPrefs {
    let mut display = main
        .cloned()
        .or_else(|| quick.map(|layout| layout.display.clone()))
        .unwrap_or_else(default_display);
    display.sidebar_visible = false;
    display
}

fn sha256(message: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut state = [
        0x6a09e667_u32,
        0xbb67ae85,
        0x3c6ef372,
        0xa54ff53a,
        0x510e527f,
        0x9b05688c,
        0x1f83d9ab,
        0x5be0cd19,
    ];
    let bit_len = (message.len() as u64).saturating_mul(8);
    let mut data = message.to_vec();
    data.push(0x80);
    while (data.len() % 64) != 56 {
        data.push(0);
    }
    data.extend(bit_len.to_be_bytes());
    for chunk in data.chunks(64) {
        let mut words = [0_u32; 64];
        for (index, word) in chunk.chunks(4).enumerate() {
            words[index] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for index in 16..64 {
            let small0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let small1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(small0)
                .wrapping_add(words[index - 7])
                .wrapping_add(small1);
        }
        let mut working = state;
        for index in 0..64 {
            let big1 = working[4].rotate_right(6)
                ^ working[4].rotate_right(11)
                ^ working[4].rotate_right(25);
            let choice = (working[4] & working[5]) ^ (!working[4] & working[6]);
            let temp1 = working[7]
                .wrapping_add(big1)
                .wrapping_add(choice)
                .wrapping_add(K[index])
                .wrapping_add(words[index]);
            let big0 = working[0].rotate_right(2)
                ^ working[0].rotate_right(13)
                ^ working[0].rotate_right(22);
            let majority =
                (working[0] & working[1]) ^ (working[0] & working[2]) ^ (working[1] & working[2]);
            let temp2 = big0.wrapping_add(majority);
            working[7] = working[6];
            working[6] = working[5];
            working[5] = working[4];
            working[4] = working[3].wrapping_add(temp1);
            working[3] = working[2];
            working[2] = working[1];
            working[1] = working[0];
            working[0] = temp1.wrapping_add(temp2);
        }
        for (slot, word) in state.iter_mut().zip(working) {
            *slot = slot.wrapping_add(word);
        }
    }
    let mut out = [0_u8; 32];
    for (index, word) in state.iter().enumerate() {
        out[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_scoped_paths_are_rename_stable_and_separate_from_name_scoped_paths() {
        let key = phux_protocol::ids::SessionKey::new([1; 16]).unwrap();
        let keyed = session_layout_paths("private-server.sock", key);
        assert_eq!(keyed, session_layout_paths("private-server.sock", key));
        assert_ne!(
            keyed.layout,
            layout_paths("private-server.sock", "A").layout
        );
        assert_ne!(
            keyed.layout,
            layout_paths("private-server.sock", "renamed-A").layout
        );
        assert_ne!(
            keyed.layout,
            session_layout_paths("another-server.sock", key).layout
        );
    }

    #[test]
    fn key_store_imports_without_overwriting_old_or_conflicting_layouts() {
        let scratch = Scratch::new("layout-key");
        let old = scratch.join("name.json");
        let keyed = scratch.join("key.json");
        let old_value = serde_json::json!({"version": 2, "serverId": "old", "tabs": []});
        fs::write(&old, old_value.to_string()).unwrap();
        let store = FileLayoutStore::new(&keyed, Some(old.clone()));
        assert_eq!(store.read(), StoreValue::Present(old_value.clone()));
        assert!(
            !keyed.exists(),
            "reading the fallback does not migrate destructively"
        );
        for bytes in ["damaged", r#"{"version":99,"serverId":"other","tabs":[]}"#] {
            fs::write(&keyed, bytes).unwrap();
            assert!(load_layout(&store.read()).blocked);
            assert_eq!(fs::read_to_string(&keyed).unwrap(), bytes);
            assert_eq!(fs::read_to_string(&old).unwrap(), old_value.to_string());
        }
        let conflicting = serde_json::json!({"version": 2, "serverId": "other", "tabs": []});
        fs::write(&keyed, conflicting.to_string()).unwrap();
        assert_eq!(store.read(), StoreValue::Present(conflicting.clone()));
        assert_eq!(fs::read_to_string(&keyed).unwrap(), conflicting.to_string());
        assert_eq!(fs::read_to_string(&old).unwrap(), old_value.to_string());
    }

    #[test]
    fn sha256_matches_the_empty_and_abc_vectors() {
        assert_eq!(
            hex(sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    fn hex(bytes: [u8; 32]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn version_one_migrates_into_even_row_splits_and_future_versions_are_blocked() {
        let parsed = parse_layout(&serde_json::json!({
            "version": 1,
            "serverId": "s",
            "tabs": [{ "id": "t", "terminals": ["a", "b", "c"], "focusedTerminal": "b" }],
            "display": { "fontSize": 15, "optionAsAlt": true }
        }))
        .expect("v1");
        assert_eq!(parsed.version, 2);
        assert_eq!(parsed.display.font_size, 15.0);
        assert!(parsed.display.option_as_alt);
        let SavedNode::Split { ratio, second, .. } = &parsed.tabs[0].root else {
            panic!("row");
        };
        assert!((f64::from(*ratio) - 1.0 / 3.0).abs() < 1e-5);
        let SavedNode::Split { ratio, .. } = second.as_ref() else {
            panic!("inner");
        };
        assert!((ratio - 0.5).abs() < 1e-5);
        assert!(
            parse_layout(&serde_json::json!({ "version": 9, "serverId": "s", "tabs": [] }))
                .is_none()
        );
        let damaged =
            StoreValue::Present(serde_json::json!({ "version": 99, "serverId": "s", "tabs": [] }));
        assert!(load_layout(&damaged).blocked);
        assert!(!load_layout(&StoreValue::Missing).blocked);
    }

    #[test]
    fn sanitize_bounds_unknown_themes() {
        use crate::settings::{DisplayPrefs, SIDEBAR_MAX, sanitize_display};
        let clean = sanitize_display(&DisplayPrefs {
            font_size: 400.0,
            line_height: f64::NAN,
            sidebar_width: 9000.0,
            theme_id: "neon".into(),
            font_family: "   ".into(),
            terminal_opacity: 0.1,
            ..DisplayPrefs::default()
        });
        assert_eq!(clean.font_size, 28.0);
        assert_eq!(clean.line_height, DisplayPrefs::default().line_height);
        assert_eq!(clean.sidebar_width, SIDEBAR_MAX);
        assert_eq!(clean.theme_id, DisplayPrefs::default().theme_id);
        assert_eq!(clean.font_family, "Paper Mono");
        assert_eq!(clean.terminal_opacity, 0.55);
    }
    #[test]
    fn recent_folders_survive_restart_reorder_and_forget_without_exceeding_limit() {
        let scratch = Scratch::new("recent");
        let store = FileLayoutStore::new(scratch.join("projects.json"), None);
        let mut history = RecentProjects::from_store(store.clone());
        for index in 0..10 {
            history
                .record(PathBuf::from(format!("/checkouts/{index}")))
                .expect("save");
        }
        assert_eq!(history.entries().len(), 8);
        assert_eq!(history.entries()[0], Path::new("/checkouts/9"));
        assert!(!history.entries().contains(&PathBuf::from("/checkouts/0")));

        history
            .record(PathBuf::from("/checkouts/4"))
            .expect("reorder");
        assert!(history.flush().is_empty());
        let mut restarted = RecentProjects::from_store(store.clone());
        assert_eq!(restarted.entries()[0], Path::new("/checkouts/4"));
        assert_eq!(restarted.entries().len(), 8);
        restarted.forget(Path::new("/checkouts/4")).expect("forget");
        assert!(restarted.flush().is_empty());
        assert!(
            !RecentProjects::from_store(store)
                .entries()
                .contains(&PathBuf::from("/checkouts/4"))
        );
    }

    #[test]
    fn failed_rename_removes_the_staging_file_and_keeps_the_destination() {
        let scratch = Scratch::new("layout-rename");
        let path = scratch.join("layout.json");
        fs::create_dir(&path).unwrap();
        let marker = path.join("marker");
        fs::write(&marker, b"kept").unwrap();
        let store = FileLayoutStore::new(&path, None);
        assert!(store.write(&serde_json::json!(1)).is_err());
        assert_eq!(fs::read(&marker).unwrap(), b"kept");
        assert!(
            !scratch.has_staging(),
            "a failed replace must not leave a staging file"
        );
    }

    /// A unique temporary directory, removed (even when read-only) on drop.
    pub(super) struct Scratch {
        pub(super) path: PathBuf,
    }

    impl Scratch {
        pub(super) fn new(label: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "phux-desktop-{label}-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        pub(super) fn join(&self, name: &str) -> PathBuf {
            self.path.join(name)
        }

        pub(super) fn has_staging(&self) -> bool {
            fs::read_dir(&self.path).unwrap().any(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains(".next")
            })
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(metadata) = fs::metadata(&self.path) {
                let mut perms = metadata.permissions();
                perms.set_mode(perms.mode() | 0o200);
                let _ = fs::set_permissions(&self.path, perms);
            }
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
