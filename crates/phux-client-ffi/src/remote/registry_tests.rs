use super::*;

#[test]
fn exact_names_do_not_collide_with_an_elided_display_name() {
    let oversized = "a".repeat(1025);
    let valid = format!("{}…", "a".repeat(1021));
    let (_dir, registry) = fixture(&format!(
        "[[remote]]\nname='{oversized}'\nendpoint='ws://localhost:1'\n[[remote]]\nname='{valid}'\nendpoint='ws://localhost:2'\n"
    ));
    assert_eq!(registry.rows[0].name, registry.rows[1].name);
    assert_eq!(registry.rows[0].route, 5);
    assert_eq!(
        registry.rows[1].route, 1,
        "exact valid name is not a duplicate of the oversized name"
    );
    assert!(registry.resolve(1).is_ok());
}

#[test]
fn quic_trailing_slash_is_not_a_dialable_authority() {
    let (_dir, registry) = fixture("[[remote]]\nname='box'\nendpoint='quic://host:8788/'\n");
    assert_eq!(registry.rows[0].route, 5);
    assert!(registry.resolve(0).is_err());
}

#[test]
fn review_multi_at_userinfo_is_redacted_without_losing_ssh_username_routes() {
    let (_dir, registry) =
        fixture("[[remote]]\nname='box'\nendpoint='wss://user@realm:synthetic-secret@host:8787'\n");
    assert!(!registry.rows[0].endpoint.contains("synthetic-secret"));
    assert_eq!(registry.rows[0].route, 5);
    assert!(registry.resolve(0).is_err());
    let (_dir, ssh) = fixture("[[remote]]\nname='box'\nendpoint='ssh://alice@box:22'\n");
    assert_eq!(ssh.rows[0].endpoint, "ssh://alice@box:22");
    assert_eq!(ssh.rows[0].route, 2);
}

#[test]
fn review_inherited_bytes_share_the_capture_and_freshness_budget() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("config.toml");
    let layer = dir.path().join("layer.toml");
    std::fs::write(&root, "extends=['layer.toml']\n").expect("root");
    let small = "[[remote]]\nname='x'\nendpoint='ws://localhost:1'\n";
    std::fs::write(&layer, small).expect("layer");
    let capture = PhuxMachineRegistry::open(root.clone(), 10, 128);
    assert_eq!(capture.rows.len(), 1);
    std::fs::write(&layer, format!("{small}#{}", "a".repeat(2100))).expect("large layer");
    let oversized = PhuxMachineRegistry::open(root, 10, 128);
    assert!(
        oversized.contents.is_none(),
        "root-only bounds do not constrain extends I/O"
    );
    assert!(
        capture.validate(0).is_err(),
        "freshness must apply the same aggregate budget"
    );
}

#[test]
fn review_bad_endpoint_syntax_is_not_advertised_as_a_working_route() {
    for endpoint in [
        "ws://",
        "quic://:garbage",
        "wss://host:70000",
        "quic://host:0",
        "quic://[::1",
        "wss://host:notaport",
        "bogus://host",
    ] {
        let (_dir, registry) = fixture(&format!(
            "[[remote]]\nname='box'\nendpoint='{endpoint}'\n[[satellites]]\nname='sat'\nendpoint='{endpoint}'\n"
        ));
        assert!(
            registry.rows.iter().all(|row| row.route == 5),
            "invalid endpoint {endpoint}"
        );
    }
}

#[test]
fn endpoint_syntax_accepts_supported_authorities_without_network() {
    for (endpoint, route) in [
        ("ws://localhost", 1),
        ("wss://host.example:443/phux", 1),
        ("quic://[::1]:8788", 1),
        ("ssh://alice@[::1]:22", 2),
        ("ssh://my_alias", 2),
    ] {
        let (_dir, registry) = fixture(&format!("[[remote]]\nname='x'\nendpoint='{endpoint}'\n"));
        assert_eq!(registry.rows[0].route, route, "{endpoint}");
    }
    for endpoint in [
        "wss://user%3Asecret@host",
        "ssh://user:secret@host",
        "quic://[::1]:garbage",
        "ws://host..",
        "ws://-host",
    ] {
        let (_dir, registry) = fixture(&format!("[[remote]]\nname='x'\nendpoint='{endpoint}'\n"));
        assert_eq!(registry.rows[0].route, 5, "{endpoint}");
        assert!(!registry.rows[0].endpoint.contains("secret"));
    }
}

#[test]
fn display_elision_is_utf8_safe_and_never_becomes_mutation_authority() {
    let name = "界".repeat(25000);
    let (_dir, registry) = fixture(&format!(
        "[[remote]]\nname='{name}'\nendpoint='ws://localhost:1'\n"
    ));
    // Use a larger file budget: the display limit remains independent.
    let registry = PhuxMachineRegistry::open(registry.path, 10, 100_000);
    let row = &registry.rows[0];
    assert!(row.name.len() <= MAX_DISPLAY_BYTES);
    assert!(row.name.ends_with('…'));
    assert_eq!(
        registry.contents.as_ref().expect("capture").remote[0].name,
        name
    );
    assert_eq!(row.route, 5);
    assert!(registry.resolve(0).is_err());
    assert!(registry.forget(0).is_err());
}

fn fixture(raw: &str) -> (tempfile::TempDir, PhuxMachineRegistry) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    std::fs::write(&path, raw).expect("fixture");
    let registry = PhuxMachineRegistry::open(path, 100, 65536);
    (dir, registry)
}

#[test]
fn empty_user_registry_reproduction_and_missing_file_have_no_remotes() {
    // phux-2jza.6 recorded actual `host ls --json` hosts:[] on the user's Mac.
    // Reproduce its contents hermetically; local sessions are not registrations.
    let (dir, registry) = fixture("");
    assert!(registry.contents.is_some());
    assert!(registry.rows.is_empty());
    assert!(registry.message.is_empty());
    let absent = PhuxMachineRegistry::open(dir.path().join("missing"), 10, 1024);
    assert!(absent.contents.is_some());
    assert!(absent.rows.is_empty());
}

#[test]
fn all_disconnected_entries_are_listed_beyond_four_without_reading_credentials() {
    use std::fmt::Write as _;
    let mut raw = String::new();
    for i in 0..9 {
        writeln!(raw, "[[remote]]\nname='user{i}@host'\nendpoint='quic://host:8788'\ntoken-file='/nonexistent/secret'").expect("fixture");
    }
    let (_dir, registry) = fixture(&raw);
    assert_eq!(registry.rows.len(), 9);
    for (index, row) in registry.rows.iter().enumerate() {
        assert_eq!(row.name, format!("user{index}@host"));
        assert_eq!(row.route, 1);
        assert!(registry.resolve(index).is_ok());
        assert!(!row.message.contains("secret"));
    }
}

#[test]
fn roles_and_ssh_routes_are_not_invented_direct_connections() {
    let (_dir, registry) = fixture(
        "[[remote]]\nname='box'\nendpoint='ssh://me@box'\n[[satellites]]\nname='box'\nendpoint='quic://box:8788'\n[[satellites]]\nname='sleep'\nendpoint='ssh://sleep'\nenabled=false\n",
    );
    assert_eq!(
        registry
            .rows
            .iter()
            .map(|r| (r.role, r.route))
            .collect::<Vec<_>>(),
        vec![(1, 2), (2, 3), (2, 4)]
    );
    for index in 0..3 {
        assert!(registry.resolve(index).is_err());
    }
}

#[test]
fn malformed_budget_and_duplicate_failures_are_explicit_and_do_not_echo_tokens() {
    let (_dir, malformed) = fixture("[[remote]]\nname='secret-token'\nendpoint=[broken");
    assert!(malformed.contents.is_none());
    assert!(malformed.message.contains("malformed"));
    assert!(!malformed.message.contains("secret-token"));
    let (_dir, duplicate) = fixture(
        "[[remote]]\nname='x'\nendpoint='ws://localhost:1'\n[[remote]]\nname='x'\nendpoint='ws://localhost:2'\n",
    );
    assert_eq!(duplicate.rows.len(), 2);
    assert!(duplicate.rows.iter().all(|r| r.route == 5));
    assert!(duplicate.resolve(0).is_err());
    assert!(duplicate.forget(1).is_err());
    assert!(read_contents(&duplicate.path, 1, 65536).is_err());
    assert!(read_contents(&duplicate.path, 10, 8).is_err());
}

#[test]
fn external_edit_refuses_old_selection_instead_of_dialing_new_alias_destination() {
    let (_dir, registry) = fixture("[[remote]]\nname='x'\nendpoint='ws://localhost:1'\n");
    let tunnel = registry.resolve(0).expect("captured tunnel");
    std::fs::write(
        &registry.path,
        "[[remote]]\nname='x'\nendpoint='ws://localhost:2'\n",
    )
    .expect("external edit");
    assert!(registry.validate(0).is_err());
    assert!(registry.resolve(0).is_err());
    assert!(registry.forget(0).is_err());
    assert_eq!(
        tunnel.endpoint, "ws://localhost:1",
        "already resolved tunnel never retargets"
    );
}

#[test]
fn exact_forget_preserves_other_role_comments_preferences_and_token_file() {
    let (dir, registry) = fixture(
        "# keep notes\n[defaults]\nshell='fish' # my shell\n[[remote]]\nname='x'\nendpoint='ws://localhost:1'\n[[satellites]]\nname='x'\nendpoint='ssh://x'\n",
    );
    let token = dir.path().join("x.token");
    std::fs::write(&token, "secret-token").expect("token");
    registry.forget(0).expect("forget remote");
    let raw = std::fs::read_to_string(&registry.path).expect("read");
    assert!(raw.contains("# keep notes"));
    assert!(raw.contains("# my shell"));
    assert!(raw.contains("[[satellites]]"));
    assert!(!raw.contains("[[remote]]"));
    assert_eq!(
        std::fs::read_to_string(token).expect("token still exists"),
        "secret-token"
    );
    assert!(registry.forget(0).is_err());
}

#[test]
fn credential_bearing_endpoint_is_neither_displayed_nor_dialed() {
    for endpoint in [
        "wss://user:secret@host:8787",
        "wss://host:8787?token=secret",
    ] {
        let (_dir, registry) = fixture(&format!("[[remote]]\nname='box'\nendpoint='{endpoint}'\n"));
        assert!(!registry.rows[0].endpoint.contains("secret"));
        assert_eq!(registry.rows[0].route, 5);
        assert!(registry.resolve(0).is_err());
    }
}

#[test]
fn symlink_forget_is_refused_without_changing_target() {
    let (dir, registry) = fixture("[[remote]]\nname='x'\nendpoint='ssh://x'\n");
    let link = dir.path().join("link.toml");
    std::os::unix::fs::symlink(&registry.path, &link).expect("symlink");
    let linked = PhuxMachineRegistry::open(link, 10, 65536);
    assert_eq!(linked.rows.len(), 1);
    assert!(linked.forget(0).is_err());
    assert!(registry.validate(0).is_ok());
}

#[test]
fn inherited_registrations_are_visible_and_layer_edits_invalidate_capture() {
    let dir = tempfile::tempdir().expect("tempdir");
    let layer = dir.path().join("machines.toml");
    let root = dir.path().join("config.toml");
    std::fs::write(
        &layer,
        "[[remote]]\nname='inherited'\nendpoint='ws://localhost:1'\n",
    )
    .expect("layer");
    std::fs::write(&root, "extends=['machines.toml']\n").expect("root");
    let registry = PhuxMachineRegistry::open(root.clone(), 10, 65536);
    assert_eq!(registry.rows.len(), 1);
    assert!(registry.resolve(0).is_ok());
    assert!(
        registry.forget(0).is_err(),
        "root cannot remove an inherited entry"
    );
    assert_eq!(
        std::fs::read_to_string(&root).expect("root unchanged"),
        "extends=['machines.toml']\n"
    );
    std::fs::write(
        &layer,
        "[[remote]]\nname='inherited'\nendpoint='ws://localhost:2'\n",
    )
    .expect("layer edit");
    assert!(registry.validate(0).is_err());
    assert!(registry.resolve(0).is_err());
}

#[test]
fn ffi_bounds_versions_nulls_and_borrowed_records() {
    let (_dir, registry) = fixture("[[remote]]\nname='x'\nendpoint='ws://localhost:1'\n");
    let path = registry.path.to_str().expect("path");
    let mut options = PhuxMachineRegistryOptions {
        size: mem::size_of::<PhuxMachineRegistryOptions>(),
        version: crate::ABI_VERSION,
        config_path: bytes_out(path.as_bytes()),
        max_entries: 20,
        max_file_bytes: 65536,
    };
    let mut out = ptr::null_mut();
    // SAFETY: fixture owns all pointers/spans and frees returned handles once.
    unsafe {
        assert_eq!(
            phux_machine_registry_open(&raw const options, &raw mut out),
            PhuxClientResult::Ok
        );
        let mut info: PhuxMachineRegistryInfo = mem::zeroed();
        info.size = mem::size_of_val(&info);
        info.version = options.version;
        assert_eq!(
            phux_machine_registry_info(out, &raw mut info),
            PhuxClientResult::Ok
        );
        assert_eq!(info.count, 1);
        assert_eq!(info.failed, 0);
        let mut row: PhuxMachineRecord = mem::zeroed();
        row.size = mem::size_of_val(&row);
        row.version = options.version;
        assert_eq!(
            phux_machine_registry_get(out, 0, &raw mut row),
            PhuxClientResult::Ok
        );
        assert_eq!(
            std::slice::from_raw_parts(row.name.data, row.name.len),
            b"x"
        );
        assert_eq!(
            phux_machine_registry_get(out, 1, &raw mut row),
            PhuxClientResult::InvalidArgument
        );
        assert_eq!(
            phux_machine_registry_get(out, 0, ptr::null_mut()),
            PhuxClientResult::InvalidArgument
        );
        let mut tunnel = ptr::null_mut();
        assert_eq!(
            phux_machine_registry_resolve(out, 0, &raw mut tunnel),
            PhuxClientResult::Ok
        );
        super::super::phux_remote_tunnel_free(tunnel);
        phux_machine_registry_free(out);
        phux_machine_registry_free(ptr::null_mut());
        options.version += 1;
        assert_ne!(
            phux_machine_registry_open(&raw const options, &raw mut out),
            PhuxClientResult::Ok
        );
        assert!(out.is_null());
        options.version = crate::ABI_VERSION;
        options.max_file_bytes = 8;
        assert_eq!(
            phux_machine_registry_open(&raw const options, &raw mut out),
            PhuxClientResult::Ok
        );
        assert_eq!(
            phux_machine_registry_info(out, &raw mut info),
            PhuxClientResult::Ok
        );
        assert_eq!(info.count, 0);
        assert_eq!(
            info.failed, 1,
            "byte exhaustion is not an empty successful registry"
        );
        phux_machine_registry_free(out);
        options.max_file_bytes = 0;
        assert_eq!(
            phux_machine_registry_open(&raw const options, &raw mut out),
            PhuxClientResult::InvalidArgument
        );
        assert!(out.is_null());
        assert_eq!(
            phux_machine_registry_validate(ptr::null(), 0),
            PhuxClientResult::InvalidArgument
        );
    }
}
