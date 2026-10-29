use super::enroll::ensure_private_dir;
use super::*;

fn code() -> JoinCode {
    JoinCode {
        hub: "[::1]:8788".to_owned(),
        server_name: Some("hub.example".to_owned()),
        ca: format!("sha256:{}", "ab".repeat(32)),
        name: "sandbox one".to_owned(),
        ticket: "cd".repeat(32),
    }
}

#[test]
fn a_join_code_round_trips_and_hides_its_ticket() {
    let code = code();
    let link = code.to_link();
    assert!(link.starts_with(JOIN_PREFIX), "{link}");
    assert!(link.contains("name=sandbox%20one"), "{link}");
    assert_eq!(JoinCode::parse(&format!("'{link}'\n")), Ok(code.clone()));
    assert!(!format!("{code:?}").contains(&code.ticket));
}

#[test]
fn a_join_code_refuses_missing_or_bad_fields() {
    let link = code().to_link();
    for (from, to) in [
        ("hub=", "hob="),
        ("&ca=sha256:", "&ca=md5:"),
        ("enroll=cdcd", "enroll=zzcd"),
        ("name=sandbox%20one", "name=a%2Fb"),
        ("hub=[::1]:8788", "hub=[::1]:0"),
    ] {
        let broken = link.replacen(from, to, 1);
        assert!(JoinCode::parse(&broken).is_err(), "{broken}");
    }
    assert!(JoinCode::parse("https://example/connect?x=1").is_err());
    assert!(JoinCode::parse(&format!("{link}&name=again")).is_err());
}

#[test]
fn host_port_splits_bracketed_and_refuses_junk() {
    assert_eq!(split_host_port("hub:1"), Ok(("hub".to_owned(), 1)));
    assert_eq!(split_host_port("[::1]:9"), Ok(("::1".to_owned(), 9)));
    for bad in ["hub", ":1", "hub:0", "hub:x", "[::1:9", "a/b:1"] {
        assert!(split_host_port(bad).is_err(), "{bad}");
    }
}

#[test]
fn plans_refuse_relative_identities_and_skip_disabled_entries() {
    let entry = |enabled: bool, cert: &str| UplinkConfigEntry {
        name: "sandbox".to_owned(),
        hub: "hub:8788".to_owned(),
        server_name: None,
        ca_fingerprint: format!("sha256:{}", "ab".repeat(32)),
        client_cert: PathBuf::from(cert),
        client_key: PathBuf::from("/s/u.key"),
        enabled,
        mail_only: false,
    };
    let plans = plan_uplinks(&[entry(true, "/s/u.pem"), entry(false, "relative")]).unwrap();
    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].server_name, "hub");
    assert!(plan_uplinks(&[entry(true, "relative.pem")]).is_err());
}

#[test]
fn a_hub_endpoint_with_a_scheme_or_no_port_is_refused() {
    assert!(validate_hub_endpoint("hub.example:8788").is_ok());
    assert!(validate_hub_endpoint("[::1]:8788").is_ok());
    for bad in ["quic://hub:8788", "hub", "hub:", "https://hub"] {
        assert!(validate_hub_endpoint(bad).is_err(), "{bad}");
    }
}

#[test]
fn an_existing_uplinks_directory_is_made_owner_only() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("uplinks");
    ensure_private_dir(&dir).unwrap();
    let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&dir), 0o700);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    ensure_private_dir(&dir).unwrap();
    assert_eq!(mode(&dir), 0o700, "an existing directory is repaired");
    let file = root.path().join("file");
    std::fs::write(&file, b"").unwrap();
    assert!(ensure_private_dir(&file).is_err(), "a file is refused");
}

fn uplink_entry(name: &str, hub: &str, enabled: bool) -> UplinkConfigEntry {
    UplinkConfigEntry {
        name: name.to_owned(),
        // Nothing listens: each supervisor fails to read its identity
        // and backs off, which is all these tests need.
        hub: hub.to_owned(),
        server_name: None,
        ca_fingerprint: format!("sha256:{}", "ab".repeat(32)),
        client_cert: PathBuf::from("/nonexistent/u.pem"),
        client_key: PathBuf::from("/nonexistent/u.key"),
        enabled,
        mail_only: false,
    }
}

fn plans(entries: &[UplinkConfigEntry]) -> Vec<UplinkSpec> {
    plan_uplinks(entries).unwrap()
}

#[tokio::test]
async fn apply_starts_stops_and_restarts_only_what_changed() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut uplinks = Uplinks::default();
            let a = uplink_entry("a", "127.0.0.1:9", true);
            let b = uplink_entry("b", "127.0.0.1:9", true);
            assert_eq!(
                uplinks.apply(plans(std::slice::from_ref(&a))),
                (0, 0),
                "nothing runs before the runtime arms supervision"
            );

            let root = CancellationToken::new();
            uplinks.arm(None, PathBuf::from("/nonexistent.sock"), root.clone());
            assert_eq!(uplinks.apply(plans(&[a.clone(), b.clone()])), (2, 0));
            let a_token = uplinks.running["a@127.0.0.1:9"].1.clone();
            assert_eq!(uplinks.apply(plans(&[a.clone(), b.clone()])), (0, 0));
            assert!(!a_token.is_cancelled(), "an unchanged uplink keeps running");

            // A changed pin restarts `b` alone; a new hub is a new key.
            let mut b2 = b;
            b2.ca_fingerprint = format!("sha256:{}", "cd".repeat(32));
            let b_token = uplinks.running["b@127.0.0.1:9"].1.clone();
            assert_eq!(uplinks.apply(plans(&[a, b2.clone()])), (1, 1));
            assert!(b_token.is_cancelled() && !a_token.is_cancelled());
            let moved = uplink_entry("a", "127.0.0.1:10", true);
            assert_eq!(uplinks.apply(plans(&[moved, b2])), (1, 1));
            assert!(a_token.is_cancelled());

            // Disabled or removed entries stop.
            assert_eq!(
                uplinks.apply(plans(&[uplink_entry("b", "127.0.0.1:9", false)])),
                (0, 2)
            );
            assert!(uplinks.running.is_empty());
            root.cancel();
        })
        .await;
}

#[tokio::test]
async fn a_reload_applies_the_source_and_keeps_running_uplinks_on_failure() {
    use std::sync::Mutex;
    tokio::task::LocalSet::new()
        .run_until(async {
            let entries = Arc::new(Mutex::new(Ok(vec![uplink_entry("a", "127.0.0.1:9", true)])));
            let source = {
                let entries = Arc::clone(&entries);
                UplinkSource::new(move || entries.lock().unwrap().clone())
            };
            let state = crate::state::SharedState::new();
            let root = CancellationToken::new();
            let running = |state: &crate::state::SharedState| {
                state.with(|s| s.uplinks().running.keys().cloned().collect::<Vec<_>>())
            };
            state.with_mut(|s| {
                s.uplinks_mut().arm(
                    Some(source),
                    PathBuf::from("/nonexistent.sock"),
                    root.clone(),
                );
            });
            reload_uplinks(&state);
            assert_eq!(running(&state), ["a@127.0.0.1:9"]);

            *entries.lock().unwrap() = Err("config.toml is unreadable".to_owned());
            reload_uplinks(&state);
            assert_eq!(
                running(&state),
                ["a@127.0.0.1:9"],
                "a failed read keeps them"
            );

            *entries.lock().unwrap() = Ok(vec![uplink_entry("a", "relative", true)]);
            reload_uplinks(&state);
            assert_eq!(
                running(&state),
                ["a@127.0.0.1:9"],
                "an invalid entry keeps them"
            );

            *entries.lock().unwrap() = Ok(vec![uplink_entry("b", "127.0.0.1:9", true)]);
            reload_uplinks(&state);
            assert_eq!(running(&state), ["b@127.0.0.1:9"]);

            *entries.lock().unwrap() = Ok(Vec::new());
            reload_uplinks(&state);
            assert!(running(&state).is_empty(), "a removed uplink stops");
            root.cancel();
        })
        .await;
}
