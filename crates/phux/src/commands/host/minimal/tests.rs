use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use std::os::unix::fs::PermissionsExt;

use super::boxes::{BoxSession, MinCli, parse_sessions};
use super::files::{BoxRegistry, Joined, MinimalDirs, render_loadout};
use super::satellite_name;
use super::service::{WatchPlan, render_plist, render_systemd};
use crate::commands::host::inbound::{self, HubFiles};

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
    assert_eq!(registry.load().unwrap(), Joined::default());
    let joined = Joined {
        boxes: BTreeMap::from([("01a1".to_owned(), "min-api".to_owned())]),
        minted: BTreeSet::from(["min-api".to_owned()]),
    };
    registry.save(&joined).unwrap();
    assert_eq!(registry.load().unwrap(), joined);
}

#[test]
fn a_registry_without_minted_owns_the_boxes_it_already_joined() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("boxes.json");
    std::fs::write(
        &path,
        "{\"schema_version\":1,\"boxes\":{\"01a1\":\"min-api\"}}\n",
    )
    .unwrap();
    let joined = BoxRegistry::at(path).load().unwrap();
    assert_eq!(joined.boxes["01a1"], "min-api");
    assert!(joined.minted.contains("min-api"));
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

fn owner_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

fn hub_files(dir: &std::path::Path) -> HubFiles {
    let config = dir.join("config.toml");
    let cert = dir.join("remote-cert.pem");
    HubFiles {
        config,
        workload: phux_server::workload::WorkloadPaths::with_overrides(
            Some(dir.join("workload-ca.pem")),
            Some(dir.join("workload-ca.key")),
            Some(dir.join("workload-keys")),
        ),
        cert,
    }
}

fn write_paired(files: &HubFiles) {
    std::fs::write(&files.config, "[policy]\nmode = \"paired\"\n").unwrap();
}

fn issue_server_cert(files: &HubFiles) {
    let issued =
        phux_server::workload::issue_server_identity(&files.workload, &["localhost".to_owned()])
            .unwrap();
    std::fs::write(&files.cert, issued.chain_pem()).unwrap();
}

fn register_satellite(files: &HubFiles, name: &str) {
    let expires = chrono::Utc::now().timestamp() + 3600;
    phux_server::workload::WorkloadRegistry::register_satellite(
        &files.workload.registry,
        b"minimal-test-key",
        name,
        expires,
    )
    .unwrap();
}

fn credential_revoked(files: &HubFiles, name: &str) -> bool {
    let text = std::fs::read_to_string(&files.workload.registry).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    doc["credentials"]
        .as_array()
        .unwrap()
        .iter()
        .find(|credential| credential["satellite"] == name)
        .is_some_and(|credential| !credential["revoked_at"].is_null())
}

fn ticket_revoked(files: &HubFiles, id: &str) -> bool {
    let path = files.workload.registry.with_file_name("enrollment-tickets");
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    doc["tickets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|ticket| ticket["id"] == id)
        .is_some_and(|ticket| !ticket["revoked_at"].is_null())
}

fn dummy_min(dir: &std::path::Path) -> MinCli {
    let path = dir.join("min");
    std::fs::write(
        &path,
        "#!/bin/sh\ncat >/dev/null\necho join refused >&2\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    MinCli::resolve(Some(&path)).unwrap()
}

fn context(dir: &std::path::Path, files: HubFiles) -> super::Context {
    super::Context {
        min: dummy_min(dir),
        hub: "host.min.internal:8788".to_owned(),
        registry: BoxRegistry::at(dir.join("boxes.json")),
        files,
    }
}

#[test]
fn minting_is_refused_without_paired_mode_or_a_ca_pin() {
    let dir = owner_dir();
    let files = hub_files(dir.path());
    let err =
        inbound::mint_code("min-api", "host.min.internal:8788", 5, &files, false).unwrap_err();
    assert!(
        err.message.contains("paired"),
        "unpaired hub must be refused: {}",
        err.message
    );

    write_paired(&files);
    let err =
        inbound::mint_code("min-api", "host.min.internal:8788", 5, &files, false).unwrap_err();
    assert!(
        err.message.contains("workload CA"),
        "a certificate with no CA pin must be refused: {}",
        err.message
    );
}

#[test]
fn a_failed_join_withdraws_the_ticket() {
    let dir = owner_dir();
    let files = hub_files(dir.path());
    write_paired(&files);
    issue_server_cert(&files);
    let ctx = context(dir.path(), files.clone());
    let session = BoxSession::new("01a17b22-a257", Some("api"), "active");
    let mut joined = Joined::default();
    let err = ctx.join(&session, "min-api", &mut joined).unwrap_err();
    assert!(!err.is_empty(), "{err}");
    let tickets =
        std::fs::read_to_string(files.workload.registry.with_file_name("enrollment-tickets"))
            .unwrap();
    let doc: serde_json::Value = serde_json::from_str(&tickets).unwrap();
    let id = doc["tickets"][0]["id"].as_str().unwrap();
    assert!(
        ticket_revoked(&files, id),
        "the minted ticket must be withdrawn: {tickets}"
    );
    let config = std::fs::read_to_string(&files.config).unwrap();
    assert!(
        !config.contains("min-api"),
        "the failed join's satellite entry must be removed: {config}"
    );
}

#[test]
fn a_failed_revoke_keeps_the_box_and_does_not_report_it_revoked() {
    let dir = owner_dir();
    let files = hub_files(dir.path());
    write_paired(&files);
    register_satellite(&files, "min-api");
    let ctx = context(dir.path(), files.clone());
    let joined = Joined {
        boxes: BTreeMap::from([("01a1".to_owned(), "min-api".to_owned())]),
        minted: BTreeSet::from(["min-api".to_owned()]),
    };
    ctx.registry.save(&joined).unwrap();
    // Owner-only access is required to revoke; drop write so the revoke fails
    // and the recorded box has to stay.
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    let mut recorded = joined.clone();
    let err = ctx.prune(&mut recorded, &BTreeSet::new()).unwrap_err();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        !err.message.contains("revoked"),
        "a failed revoke must not be reported as done: {}",
        err.message
    );
    assert_eq!(ctx.registry.load().unwrap(), joined);
    assert!(
        !credential_revoked(&files, "min-api"),
        "the credential stays until a revoke succeeds"
    );
}

#[test]
fn a_registry_read_failure_still_revokes_and_returns_the_error() {
    let dir = owner_dir();
    let mut files = hub_files(dir.path());
    write_paired(&files);
    register_satellite(&files, "min-api");
    let blocked = dir.path().join("not-config");
    std::fs::create_dir(&blocked).unwrap();
    files.config = blocked;
    let ctx = context(dir.path(), files.clone());
    let joined = Joined {
        boxes: BTreeMap::from([("01a1".to_owned(), "min-api".to_owned())]),
        minted: BTreeSet::from(["min-api".to_owned()]),
    };
    ctx.registry.save(&joined).unwrap();
    let mut recorded = joined.clone();
    let err = ctx.prune(&mut recorded, &BTreeSet::new()).unwrap_err();
    assert!(
        err.message
            .contains("could not read the satellite registry"),
        "{}",
        err.message
    );
    assert!(
        !err.message.contains("revoked"),
        "the failure must not claim the satellite was revoked: {}",
        err.message
    );
    assert_eq!(ctx.registry.load().unwrap(), joined);
    assert!(
        credential_revoked(&files, "min-api"),
        "revoke is attempted even when the registry cannot be read"
    );
}

#[test]
fn an_existing_satellite_minimal_did_not_mint_is_refused_and_not_revoked() {
    let dir = owner_dir();
    let files = hub_files(dir.path());
    write_paired(&files);
    issue_server_cert(&files);
    std::fs::write(
        &files.config,
        "[policy]\nmode = \"paired\"\n\n[[satellites]]\nname = \"min-api\"\nendpoint = \"ssh://kept\"\nenabled = true\n",
    )
    .unwrap();
    register_satellite(&files, "min-api");
    let ctx = context(dir.path(), files.clone());
    let session = BoxSession::new("01a17b22-a257", Some("api"), "active");
    let mut joined = Joined::default();
    let err = ctx.join(&session, "min-api", &mut joined).unwrap_err();
    assert!(
        err.contains("was not minted"),
        "an existing name must be refused: {err}"
    );
    let config = std::fs::read_to_string(&files.config).unwrap();
    assert!(
        config.contains("ssh://kept"),
        "the existing satellite must not be adopted: {config}"
    );
    assert!(!config.contains("inbound://"), "{config}");
    assert!(!credential_revoked(&files, "min-api"));
    let forget = super::forget_box("min-api", &joined.minted, &files).unwrap_err();
    assert!(
        forget.message.contains("was not minted"),
        "{}",
        forget.message
    );
    assert!(!credential_revoked(&files, "min-api"));
}

#[test]
fn remove_stops_when_the_watcher_cannot_be_uninstalled() {
    let dir = owner_dir();
    let files = hub_files(dir.path());
    register_satellite(&files, "min-api");
    let registry = BoxRegistry::at(dir.path().join("boxes.json"));
    let joined = Joined {
        boxes: BTreeMap::from([("01a1".to_owned(), "min-api".to_owned())]),
        minted: BTreeSet::from(["min-api".to_owned()]),
    };
    registry.save(&joined).unwrap();
    let loadout = dir.path().join("minimal");
    let err = super::finish_remove(
        Err("launchctl: permission denied".into()),
        || unreachable!("uninstall failure must stop remove"),
        &registry,
        &files,
    )
    .unwrap_err();
    assert!(
        err.message.contains("launchctl: permission denied"),
        "{}",
        err.message
    );
    assert!(err.message.contains("watcher"), "{}", err.message);
    assert_eq!(registry.load().unwrap(), joined);
    assert!(!credential_revoked(&files, "min-api"));
    assert!(!loadout.exists());
}
