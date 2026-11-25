//! Verb-level parse rules: positionals, value spellings, and ranges.

use super::*;

/// A bare `phux kill` names nothing to kill: it is a usage error at parse
/// time, not a silent exit 1 from the dispatcher.
#[test]
fn kill_requires_a_target_or_server() {
    let err = parse("phux kill").expect_err("bare `phux kill` must not parse");
    assert!(err.contains("TARGET"), "{err}");
    assert_parses(&[
        ("phux kill work", true),
        ("phux kill --server", true),
        ("phux kill work --server", false),
    ]);
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
    for (line, want_name, want_session, want_command) in [
        ("phux new foo", Some("foo"), None, &[][..]),
        (
            "phux new work -- htop -d 1",
            Some("work"),
            None,
            &["htop", "-d", "1"][..],
        ),
        // No name, command-only via `--` ⇒ auto-named session running CMD.
        ("phux new -- htop", None, None, &["htop"][..]),
        // `-s` still works and stays distinct from a positional.
        ("phux new -s flagged", None, Some("flagged"), &[][..]),
    ] {
        let Ok(Some(Command::New(crate::commands::NewArgs {
            name,
            session,
            command,
            ..
        }))) = parse(line).map(|cli| cli.command)
        else {
            panic!("{line}: expected New");
        };
        assert_eq!(name.as_deref(), want_name, "{line}");
        assert_eq!(session.as_deref(), want_session, "{line}");
        assert_eq!(command, want_command, "{line}");
    }
}

#[test]
fn new_json_accepts_repeatable_environment_assignments() {
    let cli = parse("phux new --json -s managed --env GC_SESSION=managed --env COMPLEX=a=b")
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

    // --env requires headless --json, and only KEY=VALUE.
    assert_parses(&[
        ("phux new -s interactive --env KEY=value", false),
        ("phux new --json -s managed --env MISSING_EQUALS", false),
    ]);
}

#[test]
fn spawn_and_launch_placement_flags_validate() {
    let cli = parse("phux spawn --target . --split vertical --ratio 0.3")
        .expect("explicit spawn placement parses");
    let Some(Command::Spawn(crate::commands::SpawnArgs { target, ratio, .. })) = cli.command else {
        panic!("expected Spawn");
    };
    assert_eq!(target.as_deref(), Some("."));
    assert!((ratio - 0.3).abs() < f32::EPSILON);

    assert_parses(&[
        ("phux spawn --ratio 0.3", false),
        ("phux spawn --target . --ratio 1.0", false),
        ("phux spawn --target . --satellite edge", false),
        ("phux launch codex --target . --split vertical", true),
    ]);
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
/// silent clamp; the exact boundary (4294967 * 1000 <= `u32::MAX`), an
/// explicit 0, and no `--ttl` parse.
#[test]
fn take_ttl_above_u32_ms_range_validates_at_parse_time() {
    assert_parses(&[
        ("phux take @1 --ttl 4294968", false),
        ("phux take @1 --ttl 4294967295", false),
        ("phux take @1 --ttl 4294967", true),
        ("phux take @1 --ttl 0", true),
        ("phux take @1", true),
    ]);
}

/// `new --json` without `-s NAME` is refused by clap itself; a positional
/// NAME does not satisfy it, since `--json` documents an explicit `-s`.
#[test]
fn new_json_requires_session_at_the_clap_level() {
    assert_parses(&[
        ("phux new --json", false),
        ("phux new work --json", false),
        ("phux new --json -s work", true),
        ("phux new -s work", true),
    ]);
}

#[test]
fn raw_archived_relaunch_is_an_explicit_review_flag_not_json_or_a_yes_bypass() {
    for (argv, review) in [
        (&["phux", "relaunch", "@1"][..], false),
        (&["phux", "relaunch", "@1", "--json"][..], false),
        (&["phux", "relaunch", "@1", "--approve-raw"][..], true),
        (
            &["phux", "relaunch", "@1", "--approve-raw", "--json"][..],
            true,
        ),
    ] {
        let cli = crate::parse_cli(argv.iter().copied()).unwrap();
        let Some(Command::Relaunch(args)) = cli.command else {
            panic!("relaunch");
        };
        assert_eq!(args.approve_raw, review);
    }
    assert_parses(&[
        ("phux relaunch @1 --approve-raw --yes", false),
        ("phux relaunch --approve-raw", false),
        ("phux restore --approve-raw", false),
    ]);
}
