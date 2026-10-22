use std::path::Path;

use super::*;

const PATH: &str = "/nonexistent/config.toml";

fn run(input: &str) -> CheckReport {
    check(input, Path::new(PATH)).expect("check runs")
}

fn paths(report: &CheckReport) -> Vec<&str> {
    report.findings.iter().map(|f| f.path.as_str()).collect()
}

/// Configs that must check clean: the shipped defaults (otherwise every
/// user's check reports a phux bug as their typo) and valid use of each
/// free-form surface.
#[test]
fn valid_configs_check_clean() {
    let max_history = crate::MAX_HISTORY_BYTES;
    let floor = phux_protocol::wire::frame::MAX_AGENT_SESSION_RECORD_BYTES;
    for input in [
        String::new(),
        "[keybindings]\nwhich-key = false\n\n[sidebar]\nenabled = true\n".to_owned(),
        format!("[defaults]\nhistory-bytes = {max_history}\n"),
        "[defaults]\nhistory-bytes = 1\n".to_owned(),
        format!("[limits]\nmetadata-value-bytes = {floor}\n"),
        "[[hooks.after-new-pane]]\nwhen = { session-startswith = \"work\" }\naction = \"noop\"\n".to_owned(),
        "[keybindings]\nprefix = \"C-b\"\n\n[keybindings.global]\n\"M-Enter\" = \"detach\"\n\n[keybindings.prefix-table]\nw = \"window-picker\"\n".to_owned(),
        // Parameterized-action arguments are the dispatcher's business.
        "[keybindings.prefix-table]\nR = { action = \"resize-pane\", direction = \"left\", amount = 3, made-up-arg = true }\n".to_owned(),
        "[[hooks.pane-exit]]\nwhen = { exit-code = 0 }\naction = \"noop\"\n\n\
         [[hooks.pane-exit]]\nwhen = { exit-code = \"*\" }\naction = { kind = \"run\", command = \"say done\" }\n\n\
         [[hooks.agent-state-changed]]\nwhen = { to = \"blocked\" }\naction = { kind = \"run\", command = [\"afplay\", \"/tmp/x.aiff\"] }\n".to_owned(),
        "[status]\nleft = [{ kind = \"windows\", separator = \" | \" }]\n\
         center = [\"help-hints\"]\n\
         right = [{ kind = \"session-name\", format = \"[{name}]\", style = { fg = \"red\", bold = true } }, { kind = \"time\", format = \" %H:%M\" }]\n".to_owned(),
        // `name` / `file` select a theme; every colour syntax the renderer reads.
        "[theme]\nname = \"nord\"\nfile = \"~/x/colors.toml\"\naccent = \"#1e66f5\"\n\
         chord = \"light-green\"\ndim = \"244\"\nsurface = \"reset\"\npane_title_focus = \"Bright Cyan\"\n".to_owned(),
    ] {
        let report = run(&input);
        assert!(report.is_ok(), "false positives for {input:?}: {:?}", report.findings);
    }
}

/// Each fault class: located at its full path, classified, and carrying
/// the fix-oriented text (suggestion, maximum, valid keys).
#[test]
#[allow(clippy::too_many_lines, reason = "one case table")]
fn each_mistake_is_located_classified_and_explained() {
    let floor = phux_protocol::wire::frame::MAX_AGENT_SESSION_RECORD_BYTES.to_string();
    let cases: &[(&str, &str, Fault, &[&str])] = &[
        (
            "[sidebar]\nenabledd = true\n",
            "sidebar.enabledd",
            Fault::UnknownKey,
            &["expected one of"],
        ),
        // A key removed in an earlier release is an ordinary unknown key.
        (
            "[defaults]\nrefresh-rate = 60\n",
            "defaults.refresh-rate",
            Fault::UnknownKey,
            &[],
        ),
        (
            "[keybindings]\nwhich-key = \"yes\"\n",
            "keybindings.which-key",
            Fault::BadValue,
            &[],
        ),
        (
            "[defaults]\nhistory-bytes = 134217728\n",
            "defaults.history-bytes",
            Fault::BadValue,
            &["67108864"],
        ),
        (
            "[limits]\nmetadata-value-bytes = 0\n",
            "limits.metadata-value-bytes",
            Fault::BadValue,
            &[&floor],
        ),
        (
            "[keybindings.prefix-table]\nq = \"kill-pain\"\n",
            "keybindings.prefix-table.q",
            Fault::UnknownName,
            &["unknown action `kill-pain` (did you mean `kill-pane`?)"],
        ),
        (
            "[keybindings.prefix-table]\n\"q-\" = \"kill-pane\"\n",
            "keybindings.prefix-table.q-",
            Fault::BadChord,
            &[],
        ),
        (
            "[keybindings]\nprefix = \"Ctrl-a\"\n",
            "keybindings.prefix",
            Fault::BadChord,
            &[],
        ),
        (
            "[keybindings.prefix-table]\nR = { action = \"resize-pain\", direction = \"left\" }\n",
            "keybindings.prefix-table.R",
            Fault::UnknownName,
            &["`resize-pane`"],
        ),
        (
            "[keybindings.prefix-table]\n\"y\" = \"copy-mode\"\n\"y x\" = \"kill-pane\"\n",
            "keybindings.prefix-table.\"y x\"",
            Fault::BadChord,
            &["ambiguous prefix"],
        ),
        (
            "[[hooks.pane-exited]]\naction = \"noop\"\n",
            "hooks.pane-exited",
            Fault::UnknownName,
            &["unknown hook event `pane-exited` (did you mean `pane-exit`?)"],
        ),
        (
            "[[hooks.pane-exit]]\nwhen = { exitcode = 0 }\naction = { kind = \"run\", command = \"true\" }\n",
            "hooks.pane-exit[0].when.exitcode",
            Fault::UnknownName,
            &[
                "context keys are exit-code, terminal-id",
                "did you mean `exit-code`?",
            ],
        ),
        // `-startswith` strips before the lookup; the full spelling is reported.
        (
            "[[hooks.after-new-pane]]\nwhen = { cwd-startswith = \"/x\" }\naction = \"noop\"\n",
            "hooks.after-new-pane[0].when.cwd-startswith",
            Fault::UnknownName,
            &["unknown when key `cwd-startswith`"],
        ),
        (
            "[[hooks.pane-exit]]\naction = { kind = \"run\", command = \"true\" }\n\n\
          [[hooks.pane-exit]]\naction = { kind = \"message\", text = \"bye\" }\n",
            "hooks.pane-exit[1].action",
            Fault::DeadAction,
            &["action `message` never executes server-side"],
        ),
        (
            "[[hooks.pane-exit]]\naction = { kind = \"run\", command = [] }\n",
            "hooks.pane-exit[0].action",
            Fault::DeadAction,
            &["no usable `command`"],
        ),
        (
            "[status]\nleft = [\"windws\"]\n",
            "status.left[0]",
            Fault::UnknownName,
            &["unknown widget kind `windws` (did you mean `windows`?)"],
        ),
        (
            "[status]\nright = [{ kind = \"time\", formt = \"%H\" }]\n",
            "status.right[0]",
            Fault::BadValue,
            &[
                "widget time",
                "unknown option `formt`",
                "did you mean `format`?",
            ],
        ),
        (
            "[status]\ncenter = [{ kind = \"session-name\", style = { colour = \"red\" } }]\n",
            "status.center[0]",
            Fault::BadValue,
            &["`style` must be a style table"],
        ),
        (
            "[theme]\nacent = \"#123456\"\n",
            "theme.acent",
            Fault::UnknownName,
            &["unknown theme slot `acent` (did you mean `accent`?)"],
        ),
        (
            "[theme]\naccent = \"lime\"\n",
            "theme.accent",
            Fault::BadValue,
            &["`lime` is not a colour", "`#rrggbb`"],
        ),
    ];
    for &(input, path, fault, needles) in cases {
        let report = run(input);
        assert_eq!(paths(&report), vec![path], "{input:?}");
        let finding = &report.findings[0];
        assert_eq!(finding.fault, fault, "{input:?}");
        for needle in needles {
            assert!(
                finding.message.contains(needle),
                "{input:?}: {}",
                finding.message
            );
        }
        // Schema and semantic findings alike name the file to open.
        assert_eq!(
            finding.source,
            Some(LayerSource::User(PATH.into())),
            "{input:?}: origin was {}",
            finding.origin()
        );
    }
}

/// Every mistake in one pass, not one per edit-run cycle.
#[test]
fn every_finding_is_reported_in_one_pass() {
    let report = run(
        "[sidebar]\nenabledd = true\nwidht = 4\n\n[keybindings]\nwich-key = true\nwhich-key = \"yes\"\n",
    );
    let found = paths(&report);
    for want in [
        "sidebar.enabledd",
        "sidebar.widht",
        "keybindings.wich-key",
        "keybindings.which-key",
    ] {
        assert!(found.contains(&want), "missing {want} in {found:?}");
    }
    assert!(!report.truncated);

    let report = run(
        "[status]\nleft = [\"windws\", { kind = \"time\", formt = \"%H\" }]\nright = [\"not-even-close\"]\n",
    );
    assert_eq!(
        paths(&report),
        vec!["status.left[0]", "status.left[1]", "status.right[0]"]
    );
}

/// "No findings" must never be said about a file that was never read.
#[test]
fn unparseable_toml_is_an_error_not_a_clean_report() {
    assert!(check("this is not = = toml\n", Path::new(PATH)).is_err());
}
