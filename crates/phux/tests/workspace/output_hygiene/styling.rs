/// The same environment policy governs stderr diagnostics and stdout help.
#[test]
fn human_output_color_policy_matches_help_in_pipes() {
    for (vars, colored) in [
        (vec![], false),
        (vec![("CLICOLOR_FORCE", "1")], true),
        (vec![("CLICOLOR_FORCE", "0")], false),
        (vec![("CLICOLOR_FORCE", "")], false),
        (vec![("CLICOLOR", "0")], false),
        (vec![("CLICOLOR", "1")], false),
        (vec![("CLICOLOR_FORCE", "1"), ("CLICOLOR", "0")], true),
        (vec![("NO_COLOR", "1"), ("CLICOLOR_FORCE", "1")], false),
        (vec![("NO_COLOR", ""), ("CLICOLOR_FORCE", "1")], true),
        (vec![("TERM", "dumb"), ("CLICOLOR_FORCE", "1")], false),
    ] {
        for args in [["help", "ls"], ["help", "not-a-topic"]] {
            let mut cmd = phux();
            for key in ["NO_COLOR", "CLICOLOR_FORCE", "CLICOLOR", "TERM"] {
                cmd.env_remove(key);
            }
            cmd.envs(vars.iter().copied());
            let result = cmd.args(args).output().expect("run color probe");
            let text = if args[1] == "ls" {
                result.stdout
            } else {
                result.stderr
            };
            assert_eq!(
                text.contains(&0x1b),
                colored,
                "{vars:?} {args:?}: {}",
                String::from_utf8_lossy(&text)
            );
        }
    }
}

#[test]
fn invalid_invocations_have_motif_without_coloring_message() {
    let result = phux()
        .env("CLICOLOR_FORCE", "1")
        .env_remove("NO_COLOR")
        .env("TERM", "xterm")
        .arg("not-a-command")
        .output()
        .expect("invalid invocation");
    assert_eq!(result.status.code(), Some(2));
    assert!(result.stdout.is_empty());
    let text = String::from_utf8(result.stderr).unwrap();
    assert!(
        text.starts_with(
            "\x1b[1mphux\x1b[0m: \x1b[31merror\x1b[0m: unrecognized subcommand 'not-a-command'\n"
        ),
        "{text:?}"
    );
    assert!(
        text.contains("Usage: phux"),
        "parser's actionable usage must remain: {text}"
    );
}

#[test]
fn json_usage_errors_are_one_unstyled_line_even_when_color_is_forced() {
    for args in [
        vec!["ls", "--json", "--not-a-flag"],
        vec!["ls", "--json=true", "--not-a-flag"],
        vec!["ls", "--json=false", "--not-a-flag"],
        vec!["ls", "--json=", "--not-a-flag"],
        vec!["--capabilities", "--json", "extra"],
        vec!["server", "--json"],
    ] {
        let result = phux()
            .env("CLICOLOR_FORCE", "1")
            .env_remove("NO_COLOR")
            .args(&args)
            .output()
            .expect("JSON usage error");
        assert_eq!(result.status.code(), Some(2), "{args:?}");
        assert!(result.stdout.is_empty(), "{args:?}");
        assert!(!result.stderr.contains(&0x1b), "{args:?}");
        let text = String::from_utf8(result.stderr).unwrap();
        assert_eq!(text.lines().count(), 1, "{args:?}: {text}");
        let doc: serde_json::Value = serde_json::from_str(&text).expect("pure JSON stderr");
        assert_eq!(doc["error"]["code"], "usage_error");
        assert_eq!(doc["exit_code"], 2);
        assert!(!doc["remedy"].as_str().unwrap().is_empty());
    }
}

#[test]
fn human_info_success_and_doctor_share_labels() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("absent.sock");
    let stopped = phux()
        .args(["kill", "--server", "--socket"])
        .arg(&socket)
        .output()
        .unwrap();
    assert!(stopped.status.success());
    assert!(
        String::from_utf8_lossy(&stopped.stderr).starts_with("phux: info: no server running at ")
    );

    // Warning labels are covered by the real partial-fleet fixture. There
    // are no deprecated CLI spellings to exercise after their retirement.

    let doctor = phux()
        .args(["doctor", "--socket"])
        .arg(&socket)
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    let text = String::from_utf8(doctor.stdout).unwrap();
    assert!(
        text.lines().any(|line| line.starts_with("phux: ok: ")),
        "{text}"
    );
    assert!(
        text.lines().any(|line| line.starts_with("phux: warn: ")),
        "{text}"
    );
    assert!(
        text.lines().any(|line| line.starts_with("  hint: ")),
        "{text}"
    );
    assert!(!text.contains('\x1b'));
}

#[test]
fn json_key_material_refusal_never_echoes_or_styles_the_secret() {
    let out = phux()
        .args([
            "workload",
            "enroll",
            "--json",
            "-----BEGIN PRIVATE KEY-----",
        ])
        .env("CLICOLOR_FORCE", "1")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let text = String::from_utf8(out.stderr).unwrap();
    assert_eq!(text.lines().count(), 1);
    assert!(!text.contains("PRIVATE KEY"));
    assert!(!text.contains('\x1b'));
    let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(doc["error"]["code"], "workload");
    assert_eq!(doc["exit_code"], 2);
}

/// Legacy prose failures under --json retain their shape and status, but
/// forced terminal styling must not leak into their machine-mode stderr.
#[test]
fn json_legacy_errors_remain_unstyled_when_color_is_forced() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("phux");
    std::fs::create_dir(&config).unwrap();
    std::fs::write(config.join("config.toml"), "[defaults\n").unwrap();
    // Attached values are presence flags in usage-rs, not boolean setters.
    for json_flag in [
        "--json",
        "--json=true",
        "--json=false",
        "--json=",
        "--json=other",
    ] {
        for (args, status, message) in [
            (
                vec!["pair", json_flag, "--expires", "nonsense"],
                2,
                "phux: error: pair: invalid --expires duration: \"nonsense\"\n",
            ),
            (
                vec!["config", "show", "--layers", json_flag],
                1,
                "phux: error: ",
            ),
        ] {
            let out = phux()
                .env("CLICOLOR_FORCE", "1")
                .env_remove("NO_COLOR")
                .env("TERM", "xterm")
                .env("XDG_CONFIG_HOME", dir.path())
                .args(&args)
                .output()
                .unwrap();
            assert_eq!(out.status.code(), Some(status), "{args:?}");
            assert!(out.stdout.is_empty(), "{args:?}");
            assert!(!out.stderr.contains(&0x1b), "{args:?}");
            let text = String::from_utf8(out.stderr).unwrap();
            if args[0] == "pair" {
                assert_eq!(text, message);
            } else {
                assert_eq!(
                    text,
                    format!(
                        "{message}{}: 1:10: unclosed table, expected `]`\n",
                        config.join("config.toml").display()
                    )
                );
            }
        }
    }
}
