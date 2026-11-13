//! Parsing what the CLI says during onboarding.

use super::*;

#[test]
fn shims_name_only_the_agents_now_launching_in_phux() {
    let out = br#"{"schema":"phux.agent-shims/v1","action":"install","agents":[
        {"agent":"claude","state":"reinstalled"},
        {"agent":"codex","state":"installed"},
        {"agent":"opencode","state":"skipped","reason":"not on PATH"}]}"#;
    assert_eq!(parse_shims(out).unwrap(), ["claude", "codex"]);
    assert!(parse_shims(b"usage: phux agent").is_err());
}

#[test]
fn bringing_your_setup_is_skipped_without_an_agent() {
    assert_eq!(Step::Agents.next(false), Some(Step::Done));
    assert_eq!(Step::Done.back(false), Some(Step::Agents));
    assert_eq!(Step::Agents.next(true), Some(Step::Migrate));
    assert_eq!(Step::Done.back(true), Some(Step::Migrate));
    assert_eq!(Step::Welcome.back(true), None);
    assert_eq!(Step::Done.next(true), None);
    for step in Step::ALL {
        assert_eq!(Step::from_id(step.id()), Some(step));
    }
    assert_eq!(Step::from_id("gone"), None);
}

#[test]
fn skills_read_per_agent_and_tolerate_shapes() {
    let status = br#"[
        {"agent":"claude","detected":true,
         "skill":{"path":"/h/.claude/skills/phux/SKILL.md","state":"installed"},
         "hook":{"state":"installed"},"extra":1},
        {"agent":"codex","detected":true,"skill":{"state":"missing"},
         "hook":{"state":"skipped","reason":"codex has no hooks"}},
        {"agent":"opencode","detected":false,"skill":{"state":"missing"}},
        {"detected":true}
    ]"#;
    let skills = parse_skills(status).unwrap();
    assert_eq!(skills.len(), 3, "a row without an agent is dropped");
    assert!(skills[0].installed());
    assert!(!skills[1].installed());
    assert_eq!(skills[0].label(), "Installed");
    assert_eq!(skills[1].label(), "Not installed");
    assert_eq!(
        skills[1].hook_detail().as_deref(),
        Some("Hook: codex has no hooks")
    );
    assert_eq!(skills[0].hook_detail().as_deref(), Some("Hook: installed"));
    let stale = AgentSkill {
        skill: Some("stale".into()),
        ..AgentSkill::default()
    };
    assert_eq!(stale.label(), "Needs update");
    assert_eq!(stale.hook_detail(), None);
    assert_eq!(migrators(&skills), ["claude", "codex"]);
    let wrapped = br#"{"agents":[{"agent":"claude","skill":{"state":"updated"}}]}"#;
    let skills = parse_skills(wrapped).unwrap();
    assert!(skills[0].installed() && !skills[0].detected);
    assert!(
        migrators(&skills).is_empty(),
        "undetected agents migrate nothing"
    );
    assert!(parse_skills(b"usage: phux skill").is_err());
}

#[test]
fn permissions_count_what_is_granted() {
    let status = br#"{"responsible_path":"/Apps/phux Server.app","services":[
        {"service":"full_disk_access","status":"granted"},
        {"service":"screen_recording","status":"denied"},
        {"service":"accessibility","status":"not_determined"},
        {"service":"camera","status":"granted"}]}"#;
    let perms = parse_permissions(status).unwrap();
    assert_eq!(perms.granted(), 1);
    assert_eq!(perms.grant(Service::ScreenRecording), Grant::Denied);
    assert_eq!(perms.grant(Service::Accessibility), Grant::NotDetermined);
    assert_eq!(
        perms.responsible_path.as_deref(),
        Some("/Apps/phux Server.app")
    );
    let shipped = br#"{"executable":"/x/phux","responsible_path":"/Apps/phux Server.app",
        "bundle":"/S/Current/phux Server.app",
        "permissions":[{"service":"screen_recording","status":"granted"}]}"#;
    let perms = parse_permissions(shipped).unwrap();
    assert_eq!(perms.grant(Service::ScreenRecording), Grant::Granted);
    assert_eq!(
        perms.responsible_path.as_deref(),
        Some("/S/Current/phux Server.app")
    );
    let bare = br#"[{"service":"accessibility","status":"granted"}]"#;
    let perms = parse_permissions(bare).unwrap();
    assert_eq!(perms.granted(), 1);
    assert_eq!(perms.grant(Service::FullDiskAccess), Grant::Unknown);
    assert_eq!(perms.responsible_path, None);
}

#[test]
fn the_next_permission_and_who_asks_for_it() {
    let perms = parse_permissions(
        br#"{"responsible_path":"/Applications/Phux Rune.app","permissions":[
        {"service":"full_disk_access","status":"granted"},
        {"service":"screen_recording","status":"not_determined"},
        {"service":"accessibility","status":"denied"}]}"#,
    )
    .unwrap();
    assert_eq!(perms.missing(), Some(Service::ScreenRecording));
    let all = Permissions {
        services: Service::ALL.map(|s| (s, Grant::Granted)).to_vec(),
        ..Permissions::default()
    };
    assert_eq!(all.missing(), None);

    let rune = Path::new("/Applications/Phux Rune.app/Contents/MacOS/phux-rune");
    assert!(perms.prompts_here(Service::ScreenRecording, rune));
    assert!(
        !perms.prompts_here(Service::Accessibility, rune),
        "asked once already: Settings"
    );
    assert!(
        !perms.prompts_here(Service::FullDiskAccess, rune),
        "no prompt exists"
    );
    let other = Path::new("/Applications/Other.app/Contents/MacOS/other");
    assert!(
        !perms.prompts_here(Service::ScreenRecording, other),
        "the prompt would name the wrong app"
    );
    let bare = Permissions {
        responsible_path: Some("/usr/local/bin".into()),
        ..perms.clone()
    };
    assert!(!bare.prompts_here(Service::ScreenRecording, Path::new("/usr/local/bin/x")));
}

#[test]
fn the_opened_bundle_comes_from_json_or_the_last_path() {
    assert_eq!(
        parse_opened(b"Opening Full Disk Access\n/Apps/phux Server.app\n").as_deref(),
        Some("/Apps/phux Server.app")
    );
    assert_eq!(
        parse_opened(br#"{"bundle_path":"/B.app"}"#).as_deref(),
        Some("/B.app")
    );
    assert_eq!(parse_opened(b"opened\n"), None);
    assert_eq!(parse_opened(b""), None);
}

#[test]
fn a_migration_names_where_its_agent_runs() {
    let out = br#"{"sources":["tmux",{"name":"ghostty","path":"/x"}],"agent":"claude",
        "config_path":"/h/.config/phux/config.kdl",
        "launched":{"session":"migrate","pane":"@12"},"prompt_bytes":4096}"#;
    let m = parse_migration(out).unwrap();
    assert_eq!(m.agent.as_deref(), Some("claude"));
    assert_eq!(m.session.as_deref(), Some("migrate"));
    assert_eq!(m.pane, Some(12));
    assert_eq!(m.sources, ["tmux", "ghostty"]);
    let numeric = parse_migration(br#"{"launched":{"pane":7}}"#).unwrap();
    assert_eq!((numeric.pane, numeric.session), (Some(7), None));
    let none = parse_migration(br#"{"launched":null}"#).unwrap();
    assert_eq!(none, Migration::default());
}

#[test]
fn an_old_cli_reads_as_unsupported() {
    assert!(unsupported(
        "`phux skill setup --json` failed: error: unrecognized subcommand 'setup'"
    ));
    assert!(unsupported("error: unexpected argument '--json' found"));
    assert!(!unsupported("permission denied"));
    assert!(!unsupported(
        r#"{"error":{"message":"claude is not installed"}}"#
    ));
}

#[test]
fn a_session_is_named_after_the_folder() {
    let taken = vec!["james".to_owned()];
    assert_eq!(session_name(Path::new("/Users/me"), &[]), "me");
    assert_eq!(session_name(Path::new("/Users/james"), &taken), "james-2");
    assert_eq!(session_name(Path::new("/home/a b@c"), &[]), "a-b-c");
    assert_eq!(session_name(Path::new("/"), &[]), "home");
}
