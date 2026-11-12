//! `phux host color`: override, or clear, the colour clients draw a host in
//! (`[[remote]] color`, `[[satellites]] color`, `[host] color` for this
//! machine).

use std::path::Path;
use std::process::ExitCode;

use phux_config::host_color::{self, AUTO, HostColor};
use toml_edit::{Item, Table};

use super::super::json_err::{self, CliError, codes};
use super::super::toml_registry;
use super::HostRole;
use super::add::registry_failure;
use super::manage::find_host;

/// What `phux host color` recolours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Target<'a> {
    /// This machine: `[host] color`.
    Local,
    /// The `[[remote]]` or `[[satellites]]` entry with this name and endpoint.
    Entry {
        role: HostRole,
        name: &'a str,
        endpoint: &'a str,
    },
}

/// This machine's resolved colour, keyed by its short host name.
pub(super) fn local_color() -> HostColor {
    let color = phux_config::loader::load()
        .ok()
        .and_then(|cfg| cfg.host.color);
    host_color::resolve(&phux_config::instance::local_host_name(), color.as_deref())
}

/// `phux host color`.
pub(super) fn run_color(
    args: &[String],
    local: bool,
    role: Option<HostRole>,
    json: bool,
) -> ExitCode {
    let (name, color) = match (args, local, role) {
        ([color], true, None) => (None, color.trim()),
        ([name, color], false, _) => (Some(name.as_str()), color.trim()),
        _ => {
            let err = CliError::new(
                codes::USAGE_ERROR,
                "name a host and a colour, or pass `--local` (without `--role`) and a colour",
                "`phux host color mini violet` or `phux host color --local violet`",
            );
            return json_err::emit(json, &err, 2);
        }
    };
    if color != AUTO && host_color::index_of(color).is_none() {
        let ids: Vec<_> = host_color::PALETTE.iter().map(|(id, _)| *id).collect();
        let err = CliError::new(
            codes::USAGE_ERROR,
            format!("{color:?} is not a host colour"),
            format!("use one of {} or `{AUTO}`", ids.join(", ")),
        );
        return json_err::emit(json, &err, 2);
    }
    let (label, resolved) = match name {
        None => {
            if let Err(err) = set_color_at(&config_path(), Target::Local, color) {
                return json_err::emit(json, &registry_failure(err), 1);
            }
            (phux_config::instance::local_host_name(), local_color())
        }
        Some(name) => {
            let row = match find_host(name, role, "color") {
                Ok(row) => row,
                Err((err, code)) => return json_err::emit(json, &err, code),
            };
            let target = Target::Entry {
                role: row.role,
                name: &row.name,
                endpoint: &row.endpoint,
            };
            if let Err(err) = set_color_at(&config_path(), target, color) {
                return json_err::emit(json, &registry_failure(err), 1);
            }
            // Re-read: clearing one role may leave the other role's override.
            let shown = find_host(&row.name, Some(row.role), "color")
                .ok()
                .and_then(|row| row.color);
            (
                row.name.clone(),
                host_color::resolve(&row.name, shown.as_deref()),
            )
        }
    };
    if json {
        return crate::output::json(&serde_json::json!({
            "schema_version": 1,
            "host": {"name": label, "local": local, "color": resolved},
        }));
    }
    let how = if resolved.overridden {
        "override"
    } else {
        "automatic"
    };
    outln!("{label} is {} ({how}).", resolved.id);
    ExitCode::SUCCESS
}

fn config_path() -> std::path::PathBuf {
    phux_config::loader::config_path()
}

/// Write `color` (`auto` removes it) on `target` in the config at `path`,
/// under the registry lock, keeping the rest of the file as written.
pub(super) fn set_color_at(path: &Path, target: Target<'_>, color: &str) -> Result<(), String> {
    let value = (color != AUTO).then_some(color);
    let mut doc = toml_registry::edit_document(path)?;
    match target {
        Target::Local => {
            let root = doc.as_table_mut();
            let host = root
                .entry("host")
                .or_insert_with(|| Item::Table(Table::new()))
                .as_table_mut()
                .ok_or_else(|| "`host` must be a table".to_owned())?;
            toml_registry::set_or_remove(host, "color", value);
            if host.is_empty() {
                root.remove("host");
            }
        }
        Target::Entry {
            role,
            name,
            endpoint,
        } => {
            let key = match role {
                HostRole::Remote => "remote",
                HostRole::Satellite => "satellites",
            };
            let tables = toml_registry::tables_mut(&mut doc, key)?;
            let matches = toml_registry::machine_indices(tables, name, endpoint);
            let [index] = matches.as_slice() else {
                return Err("host is ambiguous or inherited; edit its source configuration".into());
            };
            let table = tables
                .get_mut(*index)
                .ok_or_else(|| "host disappeared during edit".to_owned())?;
            toml_registry::set_or_remove(table, "color", value);
        }
    }
    doc.commit()
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    const START: &str = "# my machines\n[[remote]]\nname = \"mini\" # the desk one\n\
                         endpoint = \"quic://mini:8788\"\n";

    #[test]
    fn remote_colour_round_trips_and_keeps_comments() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, START).expect("seed");
        let mini = Target::Entry {
            role: HostRole::Remote,
            name: "mini",
            endpoint: "quic://mini:8788",
        };
        set_color_at(&path, mini, "violet").expect("set");
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.contains("# the desk one") && text.contains("color = \"violet\""));
        let cfg = phux_config::loader::load_from(&path).expect("parses");
        assert_eq!(cfg.remote[0].color.as_deref(), Some("violet"));

        set_color_at(&path, mini, AUTO).expect("clear");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), START);
    }

    #[test]
    fn local_colour_lives_in_a_host_table_that_auto_removes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, START).expect("seed");
        set_color_at(&path, Target::Local, "sky").expect("set");
        let cfg = phux_config::loader::load_from(&path).expect("parses");
        assert_eq!(cfg.host.color.as_deref(), Some("sky"));
        set_color_at(&path, Target::Local, AUTO).expect("clear");
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(!text.contains("[host]"), "{text}");
    }

    #[test]
    fn satellite_colour_round_trips_and_loads() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        let start = "[[satellites]]\nname = \"box\"\nendpoint = \"quic://box:8788\"\n";
        std::fs::write(&path, start).expect("seed");
        let sat = Target::Entry {
            role: HostRole::Satellite,
            name: "box",
            endpoint: "quic://box:8788",
        };
        set_color_at(&path, sat, "navy").expect("set");
        let cfg = phux_config::loader::load_from(&path).expect("parses");
        assert_eq!(cfg.satellites[0].color.as_deref(), Some("navy"));
        set_color_at(&path, sat, AUTO).expect("clear");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), start);
    }
}
