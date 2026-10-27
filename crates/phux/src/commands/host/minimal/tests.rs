use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::boxes::{BoxSession, parse_sessions};
use super::files::{BoxRegistry, MinimalDirs, render_loadout};
use super::satellite_name;
use super::service::{WatchPlan, render_plist, render_systemd};

#[test]
fn sessions_parse_from_min_ls_json_and_skip_idless_rows() {
    let body = r#"{
        "resource_pool": {"cpu_cores": 2},
        "sessions": [
            {"id": "01a17b22-a257", "name": "api", "status": "active", "git": null},
            {"id": "01a17b27-877d", "name": "", "status": "stopped"},
            {"name": "ghost", "status": "active"}
        ],
        "daemon_version": "0.7.1"
    }"#;
    let sessions = parse_sessions(body).unwrap();
    assert_eq!(
        sessions,
        vec![
            BoxSession::new("01a17b22-a257", Some("api"), "active"),
            BoxSession::new("01a17b27-877d", None, "stopped"),
        ]
    );
    assert!(sessions[0].is_active());
    assert!(!sessions[1].is_active());
    assert!(parse_sessions("not json").is_err());
    assert!(parse_sessions(r#"{"daemon_version":"0.7.1"}"#).is_err());
}

#[test]
fn satellite_names_are_prefixed_sanitized_and_unique() {
    let none = BTreeSet::new();
    let named = BoxSession::new("01a17b22-a257", Some("api"), "active");
    assert_eq!(satellite_name(&named, &none), "min-api");
    let odd = BoxSession::new("01a17b22-a257", Some("feat/x: y"), "active");
    assert_eq!(satellite_name(&odd, &none), "min-feat-x--y");
    let unnamed = BoxSession::new("01a17b22-a257", None, "active");
    assert_eq!(satellite_name(&unnamed, &none), "min-01a17b22");
    let taken = BTreeSet::from(["min-api"]);
    assert_eq!(satellite_name(&named, &taken), "min-api-01a17b22");
    for name in [satellite_name(&odd, &none), satellite_name(&named, &taken)] {
        phux_server::workload::validate_satellite_name(&name).unwrap();
    }
}

#[test]
fn the_loadout_patches_the_binary_onto_path_and_starts_the_server() {
    let body = render_loadout(Path::new("/opt/phux \"linux\"/phux"));
    let doc: toml_edit::DocumentMut = body.parse().unwrap();
    let patch = &doc["patches"].as_array().unwrap().get(0).unwrap();
    let patch = patch.as_inline_table().unwrap();
    assert_eq!(patch.get("dest").unwrap().as_str(), Some(".local/bin/phux"));
    assert_eq!(
        patch.get("source").unwrap().as_str(),
        Some("/opt/phux \"linux\"/phux")
    );
    let hook = &doc["lifecycle_hooks"]
        .as_array_of_tables()
        .unwrap()
        .get(0)
        .unwrap();
    let activate = hook["on_activate"].as_inline_table().unwrap();
    assert_eq!(activate.get("type").unwrap().as_str(), Some("inline"));
    assert!(
        activate
            .get("value")
            .unwrap()
            .as_str()
            .unwrap()
            .contains("phux server --ensure")
    );
}

#[test]
fn default_loadouts_gain_phux_once_and_keep_the_rest_of_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let dirs = MinimalDirs::at(dir.path().to_path_buf());
    std::fs::write(
        dirs.config_file(),
        "# mine\n[loadouts]\ndefault_loadouts = [\"helix\"]\nfollow_symlinks = true\n\n[session-keys]\nleader = \"ctrl-]\"\n",
    )
    .unwrap();
    assert!(dirs.add_default_loadout("phux").unwrap());
    assert!(!dirs.add_default_loadout("phux").unwrap());
    let text = std::fs::read_to_string(dirs.config_file()).unwrap();
    let doc: toml_edit::DocumentMut = text.parse().unwrap();
    let defaults: Vec<&str> = doc["loadouts"]["default_loadouts"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(defaults, ["helix", "phux"]);
    assert!(text.starts_with("# mine\n"));
    assert_eq!(doc["session-keys"]["leader"].as_str(), Some("ctrl-]"));

    dirs.write_loadout("phux", Path::new("/bin/phux")).unwrap();
    assert!(dirs.loadout_file("phux").exists());
    assert!(dirs.remove_loadout("phux").unwrap());
    assert!(!dirs.loadout_file("phux").exists());
    let doc: toml_edit::DocumentMut = std::fs::read_to_string(dirs.config_file())
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        doc["loadouts"]["default_loadouts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(!dirs.remove_loadout("phux").unwrap());
}

#[test]
fn a_missing_minimal_config_is_created() {
    let dir = tempfile::tempdir().unwrap();
    let dirs = MinimalDirs::at(dir.path().join("minimal"));
    assert!(dirs.add_default_loadout("phux").unwrap());
    let text = std::fs::read_to_string(dirs.config_file()).unwrap();
    assert!(text.contains("default_loadouts = [\"phux\"]"), "{text}");
}

#[test]
fn a_flat_loadout_of_the_same_name_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let dirs = MinimalDirs::at(dir.path().to_path_buf());
    std::fs::create_dir_all(dir.path().join("loadouts")).unwrap();
    std::fs::write(dir.path().join("loadouts/phux.toml"), "").unwrap();
    let err = dirs
        .write_loadout("phux", Path::new("/bin/phux"))
        .unwrap_err();
    assert!(err.contains("phux.toml"), "{err}");
}

#[test]
fn the_box_registry_round_trips_and_starts_empty() {
    let dir = tempfile::tempdir().unwrap();
    let registry = BoxRegistry::at(dir.path().join("state/boxes.json"));
    assert!(registry.load().unwrap().is_empty());
    let boxes = BTreeMap::from([("01a1".to_owned(), "min-api".to_owned())]);
    registry.save(&boxes).unwrap();
    assert_eq!(registry.load().unwrap(), boxes);
}

fn plan() -> WatchPlan {
    WatchPlan {
        label: "sh.phux.minimal".to_owned(),
        argv: vec![
            "/Users/me/bin/phux".to_owned(),
            "host".to_owned(),
            "minimal".to_owned(),
            "watch".to_owned(),
            "--min".to_owned(),
            "/Users/me/.local/bin/min".to_owned(),
        ],
        env: vec![("PHUX_SOCKET".to_owned(), "/tmp/a&b $x.sock".to_owned())],
        log: PathBuf::from("/Users/me/.local/state/phux/minimal/watch.log"),
    }
}

#[test]
fn the_launchd_agent_runs_watch_and_escapes_xml() {
    let plist = render_plist(&plan());
    assert!(plist.contains("<string>sh.phux.minimal</string>"));
    assert!(plist.contains("<string>watch</string>"));
    assert!(plist.contains("<key>KeepAlive</key>\n  <true/>"));
    assert!(plist.contains("<string>/tmp/a&amp;b $x.sock</string>"));
}

#[test]
fn the_systemd_unit_runs_watch_and_quotes_its_words() {
    let unit = render_systemd(&plan());
    assert!(unit.contains(
        "ExecStart=\"/Users/me/bin/phux\" \"host\" \"minimal\" \"watch\" \"--min\" \"/Users/me/.local/bin/min\"\n"
    ));
    assert!(unit.contains("Environment=\"PHUX_SOCKET=/tmp/a&b $$x.sock\"\n"));
    assert!(unit.contains("WantedBy=default.target"));
}
