//! Frozen fixtures: what a released image wrote, held against today's types.
//!
//! Field names are a persisted format, and `#[serde(default)]` hides a rename:
//! the old file parses cleanly and the value is gone. A round trip of today's
//! type cannot see that, because it writes the new name too. A file frozen by
//! an older image can.
//!
//! A format keeps numbered fixtures under `tests/fixtures/frozen/<name>/`, and
//! they are append-only (`scripts/check-frozen-fixtures.sh`), so a sweeping
//! rename cannot rewrite the proof along with the code. [`Format::check`]:
//!
//! - reads every fixture through the real type and requires nothing lost;
//! - requires the newest to match what this image writes, so a new field
//!   fails until a fixture freezes it;
//! - requires the newest to give every field a value, or a rename of that
//!   field would prove nothing.
//!
//! To freeze a change, set `PHUX_BLESS_FROZEN=<name>` and run the test: it
//! writes the next fixture from the newest one. Edit that new file to fill
//! any field it leaves empty; it is yours until it merges.

#![allow(
    clippy::expect_used,
    clippy::missing_panics_doc,
    clippy::panic,
    reason = "test support: a failure here is a test failure"
)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Names the format whose next fixture a test run should write.
pub const BLESS_ENV: &str = "PHUX_BLESS_FROZEN";

type Reread<'a> = dyn Fn(&[u8]) -> Result<Value, String> + 'a;

/// One persisted format and how to read it.
pub struct Format<'a> {
    name: &'a str,
    dir: PathBuf,
    extension: &'a str,
    parse: fn(&[u8]) -> Value,
    render: fn(&Value) -> Vec<u8>,
    reread: Box<Reread<'a>>,
    may_be_absent: &'a [&'a str],
}

impl std::fmt::Debug for Format<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Format").field("name", &self.name).finish()
    }
}

/// A JSON format read as `T`.
#[must_use]
pub fn json<T: Serialize + DeserializeOwned>(name: &str) -> Format<'_> {
    custom_json(name, |bytes| {
        let value: T = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        serde_json::to_value(value).map_err(|error| error.to_string())
    })
}

/// A JSON format whose reader is not plain `serde_json`.
#[must_use]
pub fn custom_json<'a>(
    name: &'a str,
    reread: impl Fn(&[u8]) -> Result<Value, String> + 'a,
) -> Format<'a> {
    Format {
        name,
        dir: crate_dir().join("tests/fixtures/frozen").join(name),
        extension: "json",
        parse: |bytes| serde_json::from_slice(bytes).expect("fixture is JSON"),
        render: |value| serde_json::to_vec_pretty(value).expect("render JSON"),
        reread: Box::new(reread),
        may_be_absent: &[],
    }
}

/// A TOML format read as `T`.
#[must_use]
pub fn toml<T: Serialize + DeserializeOwned>(name: &str) -> Format<'_> {
    Format {
        name,
        dir: crate_dir().join("tests/fixtures/frozen").join(name),
        extension: "toml",
        parse: |bytes| {
            let text = std::str::from_utf8(bytes).expect("fixture is UTF-8");
            ::toml::from_str(text).expect("fixture is TOML")
        },
        render: |value| {
            ::toml::to_string_pretty(value)
                .expect("render TOML")
                .into_bytes()
        },
        reread: Box::new(|bytes| {
            let text = std::str::from_utf8(bytes).map_err(|error| error.to_string())?;
            let value: T = ::toml::from_str(text).map_err(|error| error.to_string())?;
            serde_json::to_value(value).map_err(|error| error.to_string())
        }),
        may_be_absent: &[],
    }
}

impl<'a> Format<'a> {
    /// Keep the fixtures in `dir`, relative to the crate, instead of
    /// `tests/fixtures/frozen/<name>/`.
    #[must_use]
    pub fn in_dir(mut self, dir: &str) -> Self {
        self.dir = crate_dir().join(dir);
        self
    }

    /// Paths, indices dropped, this format's own reader refuses to see
    /// filled, so the newest fixture cannot give them a value.
    #[must_use]
    pub const fn may_be_absent(mut self, paths: &'a [&'a str]) -> Self {
        self.may_be_absent = paths;
        self
    }

    /// Every fixture's bytes, oldest first, with its file name.
    #[must_use]
    pub fn fixtures(&self) -> Vec<(String, Vec<u8>)> {
        let paths = fixture_paths(&self.dir);
        assert!(!paths.is_empty(), "{}: no frozen fixtures", self.name);
        paths
            .into_iter()
            .map(|path| {
                let name = path.file_name().expect("file").to_string_lossy();
                (name.into_owned(), std::fs::read(&path).expect("read"))
            })
            .collect()
    }

    /// Hold today's type to every frozen fixture; see the crate docs.
    pub fn check(&self) {
        let fixtures = self.fixtures();
        for (name, bytes) in &fixtures {
            let mut lost = Vec::new();
            lost_values(&(self.parse)(bytes), &self.read(name, bytes), "", &mut lost);
            assert!(
                lost.is_empty(),
                "{}/{name}: a released image's values are lost: {lost:?}",
                self.name
            );
        }
        let (name, bytes) = fixtures.last().expect("a fixture");
        if std::env::var(BLESS_ENV).is_ok_and(|format| format == self.name) {
            self.bless(name, &self.read(name, bytes));
            return;
        }
        let frozen = (self.parse)(bytes);
        assert_eq!(
            frozen,
            self.read(name, bytes),
            "{}/{name} no longer matches what this image writes; freeze the change in a new \
             fixture ({BLESS_ENV}={})",
            self.name,
            self.name,
        );
        let mut unexercised = unexercised(&frozen);
        unexercised.retain(|path| !self.may_be_absent.contains(&path.as_str()));
        assert!(
            unexercised.is_empty(),
            "{}/{name} leaves these unexercised: {unexercised:?}",
            self.name
        );
    }

    fn read(&self, name: &str, bytes: &[u8]) -> Value {
        (self.reread)(bytes).unwrap_or_else(|error| {
            panic!(
                "{}/{name}: a released file must stay readable: {error}",
                self.name
            )
        })
    }

    /// Write the fixture after `newest`.
    fn bless(&self, newest: &str, current: &Value) {
        let number: u32 = newest
            .split('.')
            .next()
            .and_then(|stem| stem.parse().ok())
            .expect("fixtures are numbered like 0002");
        let next = self
            .dir
            .join(format!("{:04}.{}", number + 1, self.extension));
        std::fs::write(next, (self.render)(current)).expect("write fixture");
    }
}

fn crate_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("set by cargo and nextest"))
}

fn fixture_paths(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<_> = entries
        .map(|entry| entry.expect("fixture entry").path())
        .collect();
    paths.sort();
    paths
}

/// Everything in `frozen` still present and equal in `current`, by path.
fn lost_values(frozen: &Value, current: &Value, path: &str, lost: &mut Vec<String>) {
    match (frozen, current) {
        (Value::Object(frozen), Value::Object(current)) => {
            for (key, value) in frozen {
                let path = format!("{path}.{key}");
                match current.get(key) {
                    Some(current) => lost_values(value, current, &path, lost),
                    None => lost.push(path),
                }
            }
        }
        (Value::Array(frozen), Value::Array(current)) if frozen.len() == current.len() => {
            for (index, value) in frozen.iter().enumerate() {
                lost_values(value, &current[index], &format!("{path}[{index}]"), lost);
            }
        }
        (frozen, current) if frozen == current => {}
        _ => lost.push(path.to_owned()),
    }
}

/// Paths, indices dropped, that nothing in `value` fills. One filled
/// occurrence exercises the field.
fn unexercised(value: &Value) -> Vec<String> {
    let (mut filled, mut absent) = (BTreeSet::new(), BTreeSet::new());
    sort_paths(value, "", &mut filled, &mut absent);
    absent.difference(&filled).cloned().collect()
}

fn sort_paths(
    value: &Value,
    path: &str,
    filled: &mut BTreeSet<String>,
    absent: &mut BTreeSet<String>,
) {
    match value {
        Value::Null => drop(absent.insert(path.to_owned())),
        Value::Array(items) if items.is_empty() => drop(absent.insert(path.to_owned())),
        Value::Object(fields) if fields.is_empty() => drop(absent.insert(path.to_owned())),
        Value::Array(items) => {
            filled.insert(path.to_owned());
            for item in items {
                sort_paths(item, path, filled, absent);
            }
        }
        Value::Object(fields) => {
            filled.insert(path.to_owned());
            for (key, field) in fields {
                let child = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                sort_paths(field, &child, filled, absent);
            }
        }
        _ => drop(filled.insert(path.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;
    use serde_json::json;

    use super::*;

    #[test]
    fn the_loss_check_catches_a_renamed_defaulted_field() {
        let frozen = json!({"keep": 1, "old_name": {"value": 2}});
        let renamed = json!({"keep": 1, "new_name": {"value": 2}});
        let mut lost = Vec::new();
        lost_values(&frozen, &renamed, "", &mut lost);
        assert_eq!(lost, [".old_name"]);
    }

    #[test]
    fn one_filled_occurrence_exercises_a_field_and_none_does_not() {
        let value = json!({"a": [{"x": null}, {"x": 1}], "b": {"y": []}});
        assert_eq!(unexercised(&value), ["b.y"]);
    }

    #[derive(Serialize, Deserialize)]
    struct Renamed {
        #[serde(default)]
        new_name: u32,
    }

    #[test]
    #[should_panic(expected = "values are lost")]
    fn check_refuses_a_rename_end_to_end() {
        let dir = std::env::temp_dir().join(format!("phux-frozen-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join("0001.json"), r#"{"old_name": 7}"#).expect("fixture");
        let mut format = super::json::<Renamed>("renamed");
        format.dir = dir;
        format.check();
    }
}
