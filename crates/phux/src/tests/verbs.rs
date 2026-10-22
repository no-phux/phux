//! Verb-level parse rules: positionals, value spellings, and ranges.

use super::*;

/// A bare `phux kill` names nothing to kill: it is a usage error at parse
/// time, not a silent exit 1 from the dispatcher.
#[test]
fn kill_requires_a_target_or_server() {
    let err = crate::parse_cli(["phux", "kill"]).expect_err("bare `phux kill` must not parse");
    assert!(err.contains("TARGET"), "{err}");
    for argv in [
        &["phux", "kill", "work"][..],
        &["phux", "kill", "--server"][..],
    ] {
        crate::parse_cli(argv).unwrap_or_else(|err| panic!("{argv:?}: {err}"));
    }
    assert!(crate::parse_cli(["phux", "kill", "work", "--server"]).is_err());
}

/// `phux signal` takes the POSIX names a `kill(1)` user types, in any
/// case, beside its own words.
#[test]
fn signal_accepts_posix_spellings() {
    use crate::commands::SignalArg;
    for (word, want) in [
        ("terminate", SignalArg::Terminate),
        ("TERM", SignalArg::Terminate),
        ("SIGTERM", SignalArg::Terminate),
        ("int", SignalArg::Interrupt),
        ("SIGINT", SignalArg::Interrupt),
        ("STOP", SignalArg::Freeze),
        ("cont", SignalArg::Resume),
        ("SIGKILL", SignalArg::Kill),
        ("Kill", SignalArg::Kill),
    ] {
        let cli = crate::parse_cli(["phux", "signal", "@1", word]).expect(word);
        assert!(
            matches!(cli.command, Some(Command::Signal(crate::commands::SignalArgs { signal, .. })) if signal == want),
            "{word}"
        );
    }
}

/// PowerShell is spelled the way the shell is, with the old derived
/// `power-shell` word and `pwsh` still accepted.
#[test]
fn completion_accepts_every_powershell_spelling() {
    for word in ["powershell", "pwsh", "power-shell"] {
        let cli = crate::parse_cli(["phux", "completion", word]).expect(word);
        assert!(
            matches!(
                cli.command,
                Some(Command::Completion(crate::commands::CompletionArgs {
                    shell: crate::commands::CompletionShell::PowerShell
                }))
            ),
            "{word}"
        );
    }
}

/// `phux new <NAME>` must read the bare positional as the SESSION NAME,
/// not as a command to spawn (the phux-new-foo bug: `phux new foo` tried
/// to exec `foo` in an auto-named "0" session). The seed command is only
/// taken after `--`.
#[test]
fn new_positional_is_session_name_command_requires_dash_dash() {
    let cli = crate::parse_cli(["phux", "new", "foo"]).expect("`phux new foo` must parse");
    let Some(Command::New(crate::commands::NewArgs {
        name,
        session,
        command,
        ..
    })) = cli.command
    else {
        panic!("expected New");
    };
    assert_eq!(
        name.as_deref(),
        Some("foo"),
        "positional is the session name"
    );
    assert_eq!(session, None, "-s not given");
    assert!(
        command.is_empty(),
        "no command without `--`; got {command:?}"
    );

    // Name + an explicit `-- CMD …`.
    let cli = crate::parse_cli(["phux", "new", "work", "--", "htop", "-d", "1"])
        .expect("`phux new work -- htop -d 1` must parse");
    let Some(Command::New(crate::commands::NewArgs { name, command, .. })) = cli.command else {
        panic!("expected New");
    };
    assert_eq!(name.as_deref(), Some("work"));
    assert_eq!(command, vec!["htop", "-d", "1"]);

    // No name, command-only via `--` ⇒ auto-named session running CMD.
    let cli = crate::parse_cli(["phux", "new", "--", "htop"]).expect("`phux new -- htop` parses");
    let Some(Command::New(crate::commands::NewArgs { name, command, .. })) = cli.command else {
        panic!("expected New");
    };
    assert_eq!(name, None, "no positional ⇒ auto-name");
    assert_eq!(command, vec!["htop"]);

    // `-s` still works and stays distinct from a positional.
    let cli =
        crate::parse_cli(["phux", "new", "-s", "flagged"]).expect("`phux new -s flagged` parses");
    let Some(Command::New(crate::commands::NewArgs { name, session, .. })) = cli.command else {
        panic!("expected New");
    };
    assert_eq!(name, None);
    assert_eq!(session.as_deref(), Some("flagged"));
}

#[test]
fn new_json_accepts_repeatable_environment_assignments() {
    let cli = crate::parse_cli([
        "phux",
        "new",
        "--json",
        "-s",
        "managed",
        "--env",
        "GC_SESSION=managed",
        "--env",
        "COMPLEX=a=b",
    ])
    .expect("`phux new --json --env KEY=VALUE` must parse");
    let Some(Command::New(crate::commands::NewArgs { env, .. })) = cli.command else {
        panic!("expected New");
    };
    assert_eq!(
        env.iter()
            .map(|item| (item.key.as_str(), item.value.as_str()))
            .collect::<Vec<_>>(),
        vec![("GC_SESSION", "managed"), ("COMPLEX", "a=b")],
    );

    assert!(
        crate::parse_cli(["phux", "new", "-s", "interactive", "--env", "KEY=value"]).is_err(),
        "--env must require headless --json until CreateIfMissing carries environment",
    );
    assert!(
        crate::parse_cli([
            "phux",
            "new",
            "--json",
            "-s",
            "managed",
            "--env",
            "MISSING_EQUALS",
        ])
        .is_err(),
        "--env must reject values that are not KEY=VALUE",
    );
}

/// phux-foz.5: `phux config reload` parses, with and without an
/// explicit `--socket`.
#[test]
fn spawn_and_launch_placement_flags_validate() {
    let cli = crate::parse_cli([
        "phux", "spawn", "--target", ".", "--split", "vertical", "--ratio", "0.3",
    ])
    .expect("explicit spawn placement parses");
    let Some(Command::Spawn(crate::commands::SpawnArgs { target, ratio, .. })) = cli.command else {
        panic!("expected Spawn");
    };
    assert_eq!(target.as_deref(), Some("."));
    assert!((ratio - 0.3).abs() < f32::EPSILON);

    assert!(crate::parse_cli(["phux", "spawn", "--ratio", "0.3"]).is_err());
    assert!(crate::parse_cli(["phux", "spawn", "--target", ".", "--ratio", "1.0"]).is_err());
    assert!(crate::parse_cli(["phux", "spawn", "--target", ".", "--satellite", "edge"]).is_err());
    assert!(
        crate::parse_cli([
            "phux", "launch", "codex", "--target", ".", "--split", "vertical"
        ])
        .is_ok()
    );
}

/// `--ratio` on the spatial verbs now validates at parse time
/// (phux-i0e8.8.4): out-of-range or non-numeric ratios are clap usage
/// errors, not runtime failures.
#[test]
fn spatial_ratio_validates_at_parse_time() {
    for verb in ["insert-pane", "mv"] {
        for bad in ["1.5", "0", "1", "-0.2", "NaN", "bogus"] {
            assert!(
                crate::parse_cli(["phux", verb, "@1", "@2", "--ratio", bad]).is_err(),
                "{verb} --ratio {bad} must fail at clap"
            );
        }
        assert!(
            crate::parse_cli(["phux", verb, "@1", "@2", "--ratio", "0.25"]).is_ok(),
            "{verb} --ratio 0.25 must parse"
        );
    }
}

/// `take --ttl` beyond the u32 millisecond range is a usage error, not a
/// silent clamp.
#[test]
fn take_ttl_above_u32_ms_range_validates_at_parse_time() {
    assert!(
        crate::parse_cli(["phux", "take", "@1", "--ttl", "4294968"]).is_err(),
        "4294968s * 1000 overflows u32 ms and must fail at clap"
    );
    assert!(
        crate::parse_cli(["phux", "take", "@1", "--ttl", "4294967295"]).is_err(),
        "a wildly out-of-range value must fail at clap"
    );
    assert!(
        crate::parse_cli(["phux", "take", "@1", "--ttl", "4294967"]).is_ok(),
        "the exact u32-ms boundary (4294967 * 1000 <= u32::MAX) must parse"
    );
    assert!(
        crate::parse_cli(["phux", "take", "@1", "--ttl", "0"]).is_ok(),
        "0 (explicit no-TTL) must still parse"
    );
    assert!(
        crate::parse_cli(["phux", "take", "@1"]).is_ok(),
        "omitting --ttl entirely must still parse"
    );
}

/// `new --json` without `-s NAME` is refused by clap itself
/// (`requires = session` via the verb's arg group), replacing the old
/// runtime gate.
#[test]
fn new_json_requires_session_at_the_clap_level() {
    crate::parse_cli(["phux", "new", "--json"])
        .expect_err("`new --json` without -s must be a usage error");

    // A positional NAME does not satisfy the rule: `--json` documents an
    // explicit `-s`.
    assert!(
        crate::parse_cli(["phux", "new", "work", "--json"]).is_err(),
        "positional NAME must not satisfy --json's -s requirement"
    );

    assert!(
        crate::parse_cli(["phux", "new", "--json", "-s", "work"]).is_ok(),
        "`new --json -s NAME` must parse"
    );
    assert!(
        crate::parse_cli(["phux", "new", "-s", "work"]).is_ok(),
        "-s without --json stays valid"
    );
}
