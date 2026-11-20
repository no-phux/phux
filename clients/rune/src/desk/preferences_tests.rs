//! Production native factories keep persistence and validation, in private children.

use phux_client_runtime::Transport;
use rune::prefs::Prefs;

use crate::mobile::{Configuration, state, state_with_identity};
use crate::test_support::isolated_preferences;

fn configuration(endpoint: &str) -> Configuration {
    serde_json::from_value(serde_json::json!({"endpoint": endpoint})).unwrap()
}

#[test]
fn public_native_factories_load_save_and_reopen_only_private_preferences() {
    isolated_preferences(
        "desk::preferences_tests::public_native_factories_load_save_and_reopen_only_private_preferences",
        || {
            let root = std::path::PathBuf::from(std::env::var_os("RUNE_PREFS_DIR").unwrap());
            let mut seed = Prefs::open(super::PREFS_ID);
            let path = seed.path().unwrap().to_owned();
            assert!(path.starts_with(&root));
            seed.set("font_size", &15.0_f32).unwrap();
            seed.set("tour_offered", &false).unwrap();
            let before = std::fs::read(&path).unwrap();
            let configured = || {
                let mut configuration = configuration("quic://localhost:8788");
                configuration.name = Some("private mini".into());
                configuration.certificate_pin = Some("11".repeat(32));
                configuration.token = Some("22".repeat(32));
                configuration.session = Some("work".into());
                configuration
            };
            let mut first = state(configured()).unwrap();
            assert_eq!(first.prefs.path(), Some(path.as_path()));
            assert_eq!(first.font_size, 15.0);
            assert_eq!(
                std::fs::read(&path).unwrap(),
                before,
                "construction only reads"
            );
            assert_eq!(first.machines.label(), "private mini");
            assert_eq!(first.launch.session.as_deref(), Some("work"));
            assert!(first.phone.is_some());
            assert!(!first.launch.can_ensure());
            assert!(!first.launch.missing_cli());
            assert!(first.launch.phux.is_none());
            let native = first.native.as_ref().unwrap();
            assert_eq!(
                native.target.transport,
                Transport::Quic("localhost:8788".into())
            );
            assert_eq!(native.target.cert_fingerprint, Some("11".repeat(32)));
            assert_eq!(native.target.token, Some("22".repeat(32)));
            first.offer_tour();
            assert_eq!(
                first.prefs.get::<bool>("tour_offered"),
                Some(false),
                "a phone offers no keyboard tour"
            );
            first.zoom(Some(16.0));
            drop(first);
            let reopened = state_with_identity(configured(), None).unwrap();
            assert_eq!(reopened.font_size, 16.0);
            assert_eq!(reopened.prefs.get::<bool>("tour_offered"), Some(false));
            assert_eq!(reopened.prefs.path(), Some(path.as_path()));
        },
    );
}

#[test]
fn invalid_native_configuration_does_not_open_or_quarantine_preferences() {
    isolated_preferences(
        "desk::preferences_tests::invalid_native_configuration_does_not_open_or_quarantine_preferences",
        || {
            let root = std::path::PathBuf::from(std::env::var_os("RUNE_PREFS_DIR").unwrap());
            let path = root.join(super::PREFS_ID).join("prefs.json");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let malformed = b"not valid preferences";
            std::fs::write(&path, malformed).unwrap();
            assert!(state(configuration("file:///not-a-transport")).is_err());
            assert!(state_with_identity(configuration("quic://example.com:8788"), None).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), malformed);
            assert_eq!(
                std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
                1
            );
            // A valid factory really does open this file: malformed data is quarantined.
            let valid = state(configuration("quic://localhost:8788")).unwrap();
            assert_eq!(valid.font_size, crate::grid::DEFAULT_FONT_SIZE);
            assert_eq!(valid.prefs.path(), Some(path.as_path()));
            assert!(!path.exists());
            assert_eq!(
                std::fs::read(path.with_extension("json.bad")).unwrap(),
                malformed
            );
        },
    );
}
