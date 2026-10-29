/// The full declare/report/clear loop against a real server: the record
/// outranks heuristics while present and disappears cleanly on `clear`.
#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn agent_record_set_show_clear_roundtrip() {
    let server = ServerGuard::start("agent");

    // Declare identity on the session's (single) pane.
    let confirmed = server.agent(&[
        "set",
        SESSION,
        "--name",
        "reviewer",
        "--kind",
        "claude",
        "--state",
        "blocked",
        "--session",
        "wave1",
    ]);
    assert!(
        confirmed.contains("\"name\":\"reviewer\""),
        "set must echo the confirmed record: {confirmed}"
    );

    // The report comes straight from the record. Detector provenance may
    // explain the record, but no identity or state heuristic may compete with
    // it.
    let shown = server.agent(&["show", SESSION, "--json"]);
    let json: serde_json::Value = serde_json::from_str(&shown).expect("agent show JSON");
    let agent = &json["agents"][0];
    assert_eq!(agent["agent"]["label"], "reviewer", "label from record");
    assert_eq!(agent["agent"]["kind"], "claude", "kind slug mapped");
    assert_eq!(agent["state"], "blocked", "state from record");
    assert_eq!(
        agent["sources"][0]["kind"], "agent_record",
        "provenance must be the structured record: {shown}"
    );
    let sources = agent["sources"].as_array().expect("sources array");
    assert!(
        sources.iter().skip(1).all(|source| {
            source["kind"]
                .as_str()
                .is_some_and(|kind| kind.starts_with("detector_"))
        }),
        "only detector provenance may accompany the authoritative record: {shown}"
    );

    // Clear: the record is deleted and the report falls back to the
    // heuristic sources (whatever they infer, provenance is not the record).
    let cleared = server.agent(&["clear", SESSION]);
    assert!(
        cleared.trim_end().ends_with("\t-"),
        "clear must confirm the tombstone: {cleared:?}"
    );
    let shown = server.agent(&["show", SESSION, "--json"]);
    let json: serde_json::Value = serde_json::from_str(&shown).expect("agent show JSON");
    let sources = json["agents"][0]["sources"]
        .as_array()
        .expect("sources array");
    assert!(
        sources
            .iter()
            .all(|source| source["kind"] != "agent_record"),
        "after clear no source may claim the record: {shown}"
    );
}

/// phux-r82.10: `phux config agents` merges live `phux.agent/v1` records
/// into the manifest projection — a declared record overrides the static
/// manifest state (and propagates its derived attention), and clearing it
/// falls the row back to the declared manifest values.
#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn config_agents_projection_tracks_live_record() {
    let server = ServerGuard::start("agent");

    // A configured plugin manifest declaring a static "codex" agent.
    let dir = tempfile::tempdir().expect("create temp config dir");
    let plugin_dir = dir.path().join("plugin");
    std::fs::create_dir_all(&plugin_dir).expect("create plugin dir");
    let manifest = plugin_dir.join("phux-plugin.toml");
    std::fs::write(
        &manifest,
        concat!(
            "id = \"example.agent-tools\"\n",
            "name = \"Agent Tools\"\n",
            "version = \"0.1.0\"\n",
            "min_phux_version = \"0.0.2\"\n\n",
            "[[agents]]\n",
            "id = \"codex\"\n",
            "label = \"Codex\"\n",
            "state = \"idle\"\n",
            "attention = \"low\"\n",
        ),
    )
    .expect("write manifest");
    let xdg = dir.path().join("xdg");
    let config_dir = xdg.join("phux");
    std::fs::create_dir_all(&config_dir).expect("create config dir");
    std::fs::write(
        config_dir.join("config.toml"),
        format!(
            "[[plugins]]\nmanifest = \"{}\"\nenabled = true\n",
            manifest.display()
        ),
    )
    .expect("write config");
    let envs: &[(&str, &std::path::Path)] = &[("XDG_CONFIG_HOME", xdg.as_path())];

    // Declare a live blocked codex record on the session's pane; the
    // projection must report the runtime state and its derived high
    // attention instead of the declared idle/low baseline.
    server.agent(&[
        "set", SESSION, "--name", "codex", "--kind", "codex", "--state", "blocked",
    ]);
    let live = server.run_with_env(&["config", "agents", "--json"], envs);
    let json: serde_json::Value = serde_json::from_str(&live).expect("config agents JSON");
    assert_eq!(json["schema_version"], 2);
    assert_eq!(json["live"], true, "server answered: {live}");
    let agent = &json["agents"][0];
    assert_eq!(agent["id"], "codex");
    assert_eq!(agent["state"], "blocked", "runtime overrides manifest");
    assert_eq!(agent["attention"], "high", "attention propagates: {live}");
    assert_eq!(agent["source"], "runtime");
    assert_eq!(agent["declared"]["state"], "idle");
    assert_eq!(agent["runtime"]["state"], "blocked");
    assert_eq!(agent["runtime"]["asked"], false);

    // Clear the record: the projection falls back to the declared values
    // even though the server is still live.
    server.agent(&["clear", SESSION]);
    let fallback = server.run_with_env(&["config", "agents", "--json"], envs);
    let json: serde_json::Value = serde_json::from_str(&fallback).expect("config agents JSON");
    assert_eq!(json["live"], true);
    let agent = &json["agents"][0];
    assert_eq!(agent["state"], "idle", "declared fallback: {fallback}");
    assert_eq!(agent["attention"], "low");
    assert_eq!(agent["source"], "manifest");
    assert_eq!(agent["runtime"], serde_json::Value::Null);
}
