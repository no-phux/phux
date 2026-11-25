//! Help pages, `phux help`, and parse-failure reporting.

use super::*;

/// The up-front refusal is the `workload` verb's alone, and only before
/// `--`: other verbs carry key-looking text for panes and scanners.
#[test]
fn only_the_workload_verb_refuses_key_material_before_the_separator() {
    let key = "-----BEGIN PRIVATE KEY-----\nMIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQg\n-----END PRIVATE KEY-----";
    let cert_out = format!("--cert-out={key}");
    assert!(crate::parse_error::workload_argv_carries_key_material(
        &argv(&[
            "workload",
            "add-key",
            "--scope",
            "observe@global",
            &cert_out
        ])
    ));
    for passes in [
        vec!["run", "--", "grep", "PRIVATE KEY", "/dev/null"],
        vec!["send-keys", "%1", key],
        vec!["send-keys", "%1", "workload", key],
        vec!["workload", "add-key", "--", key],
    ] {
        assert!(
            !crate::parse_error::workload_argv_carries_key_material(&argv(&passes)),
            "{passes:?}"
        );
    }
    let secret = "kG-fUyzXcIZ2qVoMkH7Se-XnBIgz8qr4is4e0PFiWQ0";
    let redacted =
        crate::parse_error::redact_long_base64(&format!("error: unexpected argument '{secret}'"));
    assert_eq!(redacted, "error: unexpected argument '<redacted>'");
}

/// A JSON usage error names the missing argument in `message`, not only in
/// the remedy, and plain headers pass through untouched.
#[test]
fn json_usage_message_names_the_listed_subject() {
    let (message, rest) = crate::parse_error::complete_failure_header(
        "the following required arguments were not provided:",
        "  <TARGET>\n  <KEYS>\n\nUsage: phux tag ls <TARGET>\n",
    );
    assert_eq!(
        message,
        "the following required arguments were not provided: <TARGET>, <KEYS>"
    );
    assert_eq!(rest, "\nUsage: phux tag ls <TARGET>\n");
    let (message, rest) =
        crate::parse_error::complete_failure_header("unexpected argument 'x' found", "\nUsage\n");
    assert_eq!(message, "unexpected argument 'x' found");
    assert_eq!(rest, "\nUsage\n");
}

/// `phux help <verb...>` becomes `phux <verb...> --help`; a bare `help`
/// and anything else pass through untouched.
#[test]
fn help_verb_is_rewritten_to_the_verbs_own_help_flag() {
    for (line, rewritten) in [
        ("phux help host add", "phux host add --help"),
        ("phux help", "phux help"),
        ("phux ls help", "phux ls help"),
        ("phux --socket /s help ls", "phux --socket /s ls --help"),
    ] {
        assert_eq!(
            crate::help::rewrite_help_verb(words(line)),
            words(rewritten)
        );
    }
    assert_eq!(
        crate::help::help_word_index(&words("--socket=/s help")),
        Some(1)
    );
    assert_eq!(crate::help::help_word_index(&words("--socket /s")), None);
    assert_eq!(crate::help::help_word_index(&words("ls")), None);
}

/// Hidden verbs and aliases count: `phux help stdio-bridge` and
/// `phux help a` render pages, while a topic name is not a verb.
#[test]
fn top_level_verbs_include_hidden_ones_and_aliases() {
    for word in ["attach", "a", "stdio-bridge", "bootstrap", "list"] {
        assert!(crate::help::is_top_level_verb(word), "{word} is a verb");
    }
    for word in ["targets", "environment", "nonsense", ""] {
        assert!(
            !crate::help::is_top_level_verb(word),
            "{word} is not a verb"
        );
    }
}

#[test]
fn mission_help_offers_the_socket_the_keeper_watches() {
    let spec = Cli::spec();
    let mission = spec
        .root
        .subcommands
        .iter()
        .find(|sub| sub.cmd.name == "mission")
        .expect("mission");
    let page = crate::render_help_page(mission.cmd, true, usage::help::Style::PLAIN).expect("page");
    assert!(
        page.contains("--socket"),
        "the keeper watches this server; {page}"
    );
    let add = mission
        .subcommands
        .iter()
        .find(|sub| sub.cmd.name == "add")
        .expect("add");
    let add_page =
        crate::render_help_page(add.cmd, true, usage::help::Style::PLAIN).expect("add page");
    assert!(add_page.contains("--socket"), "{add_page}");
}

/// The coloured page and the plain page carry the same text, and the
/// footer follows the style rather than the palette.
#[test]
fn styled_root_page_strips_to_the_plain_one() {
    use usage::help::{Palette, Style};
    let plain =
        crate::render_help_page(Cli::spec().root.cmd, true, Style::PLAIN).expect("root page");
    let quiet_plain = crate::render_help_page(
        Cli::spec().root.cmd,
        true,
        Style::PLAIN.palette(Palette::DEFAULT.heading("bold")),
    )
    .expect("root page");
    assert_eq!(
        plain, quiet_plain,
        "a palette on a plain style must stay plain"
    );
    assert!(!plain.contains('\x1b'));

    let coloured =
        crate::render_help_page(Cli::spec().root.cmd, true, Style::COLOURED).expect("root page");
    assert!(coloured.contains("\x1b[1mLearn more:\x1b[0m"));
    let stripped: String = {
        let mut out = String::new();
        let mut rest = coloured.as_str();
        while let Some(at) = rest.find('\x1b') {
            out.push_str(&rest[..at]);
            let after = &rest[at..];
            let end = after.find('m').map_or(after.len(), |m| m + 1);
            rest = &after[end..];
        }
        out.push_str(rest);
        out
    };
    // The colouring pass paints `code` spans and drops their backticks,
    // so the comparison is on the text with backticks removed.
    assert_eq!(stripped.replace('`', ""), plain.replace('`', ""));

    // A subcommand page carries no footer.
    let attach = Cli::spec()
        .root
        .subcommands
        .iter()
        .find(|sub| sub.cmd.name == "attach")
        .expect("attach");
    let page = crate::render_help_page(attach.cmd, true, Style::PLAIN).expect("attach page");
    assert!(!page.contains("Learn more:"));
}

#[test]
fn root_short_help_survives_global_option_placement() {
    for argv in [
        ["-h"].as_slice(),
        ["--socket", "/tmp/phux.sock", "-h"].as_slice(),
        ["-h", "--socket=/tmp/phux.sock"].as_slice(),
    ] {
        assert!(
            crate::help::short_help_requested_in(argv),
            "missed {argv:?}"
        );
    }
    assert!(!crate::help::short_help_requested_in(["attach", "-h"]));
}

/// The hint follows where the flag was typed: before the verb it says
/// to move it; after a verb that does not take it, moving it would not
/// help, so it names the verbs that do.
#[test]
fn misplaced_flag_hint_follows_the_flag_position() {
    let hint = |argv: &[&str]| {
        let os: Vec<&std::ffi::OsStr> = argv.iter().map(std::ffi::OsStr::new).collect();
        let err = Cli::parse_from_argv(&os).expect_err("refused");
        let words: Vec<String> = argv.iter().map(|w| (*w).to_owned()).collect();
        crate::parse_error::misplaced_flag_hint(&err, &words)
    };
    let before = hint(&["phux", "--json", "ls"]).expect("hint");
    assert!(before.contains("place it after the verb"), "{before}");
    let after = hint(&["phux", "ls", "--host", "mini"]).expect("hint");
    assert!(
        after.contains("`phux ls` does not take `--host`"),
        "{after}"
    );
    assert!(!after.contains("place it after the verb"), "{after}");
    assert_eq!(hint(&["phux", "ls", "--no-such-flag"]), None);
}

/// A required flag reads as it is typed in a usage line, `--route <NAME>`,
/// not angled like a positional; values, positionals, and the `--`
/// separator keep their brackets, and prose is untouched.
#[test]
fn usage_lines_write_required_flags_bare() {
    let bare = crate::help::bare_required_flags;
    assert_eq!(
        bare("Usage: phux relay pair <--route <NAME>>\n"),
        "Usage: phux relay pair --route <NAME>\n"
    );
    assert_eq!(
        bare(
            "Usage: phux slot add <--to <PANE>> [--json] <PANE>\n       phux x <-q> <-- COMMAND>…\n"
        ),
        "Usage: phux slot add --to <PANE> [--json] <PANE>\n       phux x -q <-- COMMAND>…\n"
    );
    let coloured =
        "\x1b[1mUsage:\x1b[0m phux relay pair <\x1b[1;36m--route\x1b[0m \x1b[36m<NAME>\x1b[0m>\n";
    assert_eq!(
        bare(coloured),
        "\x1b[1mUsage:\x1b[0m phux relay pair \x1b[1;36m--route\x1b[0m \x1b[36m<NAME>\x1b[0m\n"
    );
    let prose = "Pass <--route <NAME>> to pick one.\n";
    assert_eq!(bare(prose), prose);
    let page = crate::render_help_page(
        Cli::spec()
            .root
            .subcommands
            .iter()
            .find(|sub| sub.cmd.name == "relay")
            .and_then(|relay| relay.subcommands.iter().find(|sub| sub.cmd.name == "pair"))
            .expect("relay pair")
            .cmd,
        true,
        usage::help::Style::PLAIN,
    )
    .expect("page");
    assert!(
        page.contains("Usage: phux relay pair --route <NAME>\n"),
        "{page}"
    );
}
