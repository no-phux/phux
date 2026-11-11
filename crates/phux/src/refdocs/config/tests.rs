use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use phux_config::{
    Action, Config, ConnectorConfigEntry, HookEntry, RemoteConfigEntry, SatelliteConfigEntry,
    UplinkConfigEntry, Widget,
};

use super::{SECTIONS, default_scalar_rows, page, scalar_rows};

/// A `Config` with every collection field non-empty and every option
/// set, so serialization cannot drop a top-level key (empty maps and
/// `None` options are the shapes serializers elide).
fn fully_populated_sample() -> Config {
    let mut config = Config::default();
    config.defaults.shell = Some("/bin/zsh".to_owned());
    config.defaults.spawn_on_attach = Some("htop".to_owned());
    config
        .keybindings
        .prefix_table
        .insert("x".to_owned(), Action::Bare("kill-pane".to_owned()));
    config
        .keybindings
        .global
        .insert("M-Enter".to_owned(), Action::Bare("detach".to_owned()));
    config.status.left = vec![Widget::Bare("windows".to_owned())];
    config.sidebar.enabled = true;
    config.hooks.insert(
        "pane-exit".to_owned(),
        vec![HookEntry {
            when: std::collections::BTreeMap::new(),
            action: Action::Bare("kill-pane".to_owned()),
        }],
    );
    config.plugins = vec![phux_config::plugin::PluginConfigEntry {
        manifest: PathBuf::from("/plugins/example/phux-plugin.toml"),
        enabled: true,
    }];
    config.satellites = vec![SatelliteConfigEntry {
        name: "devbox".to_owned(),
        endpoint: "quic://devbox.example:8788".to_owned(),
        enabled: true,
        token_file: Some(PathBuf::from("/tokens/devbox.token")),
        cert_fingerprint: Some("AB:CD".to_owned()),
        client_cert: None,
        client_key: None,
    }];
    config.connector = vec![ConnectorConfigEntry {
        relay: "relay.example:4433".to_owned(),
        token_file: Some(PathBuf::from("/tokens/relay.token")),
        cert_fingerprint: Some("AB:CD".to_owned()),
    }];
    config.uplink = vec![UplinkConfigEntry {
        name: "sandbox".to_owned(),
        hub: "hub.example:8788".to_owned(),
        server_name: Some("hub.example".to_owned()),
        ca_fingerprint: "sha256:ab".to_owned(),
        client_cert: PathBuf::from("/state/uplinks/uplink.sandbox.pem"),
        client_key: PathBuf::from("/state/uplinks/uplink.sandbox.key"),
        enabled: true,
        mail_only: false,
    }];
    config.remote = vec![RemoteConfigEntry {
        name: "mini".to_owned(),
        endpoint: "quic://mini.example:8788".to_owned(),
        token_file: Some(PathBuf::from("/tokens/mini.token")),
        cert_fingerprint: Some("AB:CD".to_owned()),
        tls_server_name: None,
        session: Some("main".to_owned()),
        ssh: Some("me@mini".to_owned()),
        direct: None,
        remote_phux: None,
        client_cert: Some(PathBuf::from("/state/remotes/mini.client.pem")),
        client_key: Some(PathBuf::from("/state/remotes/mini.client.key")),
        color: Some("violet".to_owned()),
    }];
    config.host.color = Some("sky".to_owned());
    config
        .theme
        .slots
        .insert("fg".to_owned(), "#cdd6f4".to_owned());
    config.experimental.predictive_echo = Some(true);
    config
}

/// Every [`TRISTATE_ROWS`] key must really be absent from the serialized
/// defaults, or the page would carry a duplicate, stale row.
#[test]
fn tristate_keys_are_really_unset_in_the_schema() {
    let defaults =
        toml::Value::try_from(Config::default()).expect("Config::default() serializes to TOML");
    let mut serialized = Vec::new();
    scalar_rows(&defaults, "", &mut serialized);
    for (key, _) in super::TRISTATE_ROWS {
        assert!(
            !serialized.iter().any(|(got, _)| got == key),
            "{key} is listed in TRISTATE_ROWS but the schema now \
             serializes a default for it; drop the hand-written row and \
             let the generated table carry it"
        );
    }
}

/// `SECTIONS` and the schema's serde key set must be identical.
#[test]
fn sections_cover_the_whole_schema() {
    let serialized = toml::Value::try_from(fully_populated_sample())
        .expect("a fully-populated Config serializes to TOML");
    let schema_keys: BTreeSet<&str> = serialized
        .as_table()
        .expect("Config serializes to a table")
        .keys()
        .map(String::as_str)
        .collect();
    let section_keys: BTreeSet<&str> = SECTIONS.iter().map(|section| section.key).collect();
    assert_eq!(
        section_keys, schema_keys,
        "refdocs::config::SECTIONS drifted from the Config schema; \
         update SECTIONS in crates/phux/src/refdocs/config.rs, then \
         run `just docs-gen` and commit the result"
    );
}

/// The embedded `default.toml` must agree with the schema defaults on every
/// scalar it covers (else reconcile, or run `just docs-gen`).
#[test]
fn the_embedded_defaults_agree_with_the_schema_defaults() {
    let parsed =
        phux_config::parse_str(phux_config::DEFAULT_CONFIG_TOML, Path::new("default.toml"))
            .expect("embedded defaults parse");
    let serialized = toml::Value::try_from(parsed).expect("parsed defaults serialize");
    let mut shipped = Vec::new();
    scalar_rows(&serialized, "", &mut shipped);

    for (key, default_value) in default_scalar_rows() {
        let shipped_value = shipped
            .iter()
            .find(|(shipped_key, _)| *shipped_key == key)
            .map_or_else(
                || panic!("default.toml round-trip lost the `{key}` scalar"),
                |(_, value)| value.as_str(),
            );
        assert_eq!(
            shipped_value, default_value,
            "`{key}`: default.toml ships {shipped_value} but the \
             schema default is {default_value}; reconcile them, then \
             run `just docs-gen`"
        );
    }
}

/// The page carries the section index, scalar rows, and the verbatim
/// defaults.
#[test]
fn config_page_renders_index_scalars_and_annotated_defaults() {
    let page = page();
    for section in SECTIONS {
        assert!(
            page.body.contains(&format!("| `{}` |", section.header)),
            "section index lost the {} row",
            section.header
        );
    }
    for row in [
        "| `defaults.history-bytes` | `10485760` |",
        "| `defaults.history-limit` | `50000` |",
        "| `keybindings.prefix` | `\"C-a\"` |",
        "| `sidebar.width` | `20` |",
        "| `status.position` | `\"top\"` |",
    ] {
        assert!(page.body.contains(row), "scalar table lost the row {row:?}");
    }
    assert!(
        page.body
            .contains(&format!("```toml\n{}```", phux_config::DEFAULT_CONFIG_TOML)),
        "the annotated default config must appear verbatim in a fenced block"
    );
}
