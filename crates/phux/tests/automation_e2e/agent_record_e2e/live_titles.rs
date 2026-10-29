/// Write an executable named `name` that sets the OSC 0/2 title `title`,
/// paints nothing else, and holds. With an empty screen, any state the server
/// derives for it comes from a `title`-scoped rule alone.
fn write_titled_agent(dir: &std::path::Path, name: &str, title: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;

    let path = dir.join(name);
    let script = format!("#!/bin/sh\nprintf '\\033[2J\\033[H\\033]2;{title}\\007'\nsleep 120\n");
    std::fs::write(&path, script).expect("write titled agent");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod titled agent");
    path
}

/// phux-4uzr: when the server derives a pane's state from a `title`-scoped
/// rule alone, `agent show` and `agent explain` must replay the manifest
/// against the same OSC title the detector read. They once read
/// `GET_STATE`'s `ResourceInfo::title`, which is the user-set title and
/// always empty here, so the explanation said nothing matched while the
/// record reported `blocked`.
#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn a_title_only_rule_is_explained_with_the_title_the_detector_read() {
    // OMP's `title-attention` rule (`^π !`) is title-only; the screen is blank.
    const TITLE: &str = "π ! e2e";

    let home = tempfile::tempdir().expect("create temp dir");
    let fake_omp = write_titled_agent(home.path(), "omp", TITLE);
    let server = start_with_env(&[("PHUX_AGENT_STARTUP_GRACE_MS", TEST_STARTUP_GRACE_MS)]);
    let terminal_id = server.spawn_pane(&fake_omp);
    let target = format!("@{terminal_id}");

    let json = server.await_agent_state(&target, "blocked", DETECT_DEADLINE);
    let agent = &json["agents"][0];
    assert_eq!(agent["agent"]["kind"], "omp", "{json}");
    assert_eq!(
        agent["title"], TITLE,
        "show must report the live OSC title: {json}"
    );
    let sources = agent["sources"].as_array().expect("sources array");
    let rule = sources
        .iter()
        .find(|source| source["kind"] == "detector_rule")
        .unwrap_or_else(|| panic!("a detector_rule source must name the title rule: {json}"));
    assert_eq!(rule["rule"], "title-attention", "{json}");
    assert_eq!(rule["region"], "title", "{json}");
    assert!(
        sources
            .iter()
            .all(|source| source["kind"] != "detector_fallback"),
        "the explanation must not claim nothing matched: {json}"
    );

    let explained = server.agent(&["explain", &target]);
    assert!(
        explained.contains("rule `title-attention` matched the `title` region"),
        "explain must name the title rule: {explained}"
    );
}

/// Write an executable that raises the ADR-0035 `phux-ask` title sentinel
/// for `deploy`, then writes the first line it reads to `answer_file`.
fn write_asker(dir: &std::path::Path, answer_file: &std::path::Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;

    let path = dir.join("asker");
    let script = format!(
        "#!/bin/sh\nprintf '\\033]2;phux-ask[deploy]:Deploy to prod??s=Yes|No\\007'\n\
         IFS= read -r answer\nprintf '%s\\n' \"$answer\" > '{}'\nsleep 120\n",
        answer_file.display()
    );
    std::fs::write(&path, script).expect("write asker");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod asker");
    path
}

/// phux-4uzr's sibling: `agent answer` and `config agents` find the
/// `phux-ask` sentinel in the live OSC title. Reading `GET_STATE`'s user-set
/// title instead, `answer` refused every live ask with `no_active_ask` and
/// `config agents` never reported `asked`.
#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn agent_answer_and_config_agents_read_the_live_ask_title() {
    let home = tempfile::tempdir().expect("create temp dir");
    let answer_file = home.path().join("answer.txt");
    let asker = write_asker(home.path(), &answer_file);
    let server = ServerGuard::start("agent");
    let terminal_id = server.spawn_pane(&asker);
    let target = format!("@{terminal_id}");

    // A barrier on the title itself, read through the verb under test's own
    // source: the asker has set it once `snapshot` reports it.
    let end = Instant::now() + DETECT_DEADLINE;
    loop {
        let shot = server.run_with_env(&["snapshot", &target, "--json"], &[]);
        let screen: serde_json::Value = serde_json::from_str(&shot).expect("snapshot JSON");
        if screen["title"]
            .as_str()
            .is_some_and(|title| title.starts_with("phux-ask[deploy]"))
        {
            break;
        }
        assert!(
            Instant::now() < end,
            "the asker never set its title: {screen}"
        );
        std::thread::sleep(RECORD_POLL);
    }

    // `config agents`: a recorded pane whose live title asks reads `asked`.
    let config = tempfile::tempdir().expect("create temp config dir");
    let manifest = config.path().join("phux-plugin.toml");
    std::fs::write(
        &manifest,
        concat!(
            "id = \"example.asker\"\n",
            "name = \"Asker\"\n",
            "version = \"0.1.0\"\n",
            "min_phux_version = \"0.0.2\"\n\n",
            "[[agents]]\n",
            "id = \"asker\"\n",
            "label = \"Asker\"\n",
        ),
    )
    .expect("write manifest");
    let xdg = config.path().join("xdg");
    std::fs::create_dir_all(xdg.join("phux")).expect("create config dir");
    std::fs::write(
        xdg.join("phux").join("config.toml"),
        format!(
            "[[plugins]]\nmanifest = \"{}\"\nenabled = true\n",
            manifest.display()
        ),
    )
    .expect("write config");
    server.agent(&["set", &target, "--name", "asker", "--kind", "asker"]);
    let live = server.run_with_env(
        &["config", "agents", "--json"],
        &[("XDG_CONFIG_HOME", xdg.as_path())],
    );
    let json: serde_json::Value = serde_json::from_str(&live).expect("config agents JSON");
    assert_eq!(
        json["agents"][0]["runtime"]["asked"], true,
        "the live phux-ask title must surface as asked: {live}"
    );

    // `agent answer`: the validated choice is typed into the asking pane.
    let answered = server.agent(&["answer", &target, "--id", "deploy", "--choice", "1"]);
    assert!(
        answered.contains("answered deploy"),
        "answer must confirm delivery: {answered}"
    );
    let end = Instant::now() + DETECT_DEADLINE;
    loop {
        if let Ok(text) = std::fs::read_to_string(&answer_file)
            && text.ends_with('\n')
        {
            assert_eq!(text, "Yes\n", "the first suggestion is the answer");
            break;
        }
        assert!(Instant::now() < end, "the asker never received the answer");
        std::thread::sleep(RECORD_POLL);
    }
}
