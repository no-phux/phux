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
        // A user key mode, entered once from the prefix table.
        "[keybindings.modes.pick]\ny = \"copy-mode\"\nq = \"exit-mode\"\n\n\
         [keybindings.mode-options.pick]\nexclusive = false\n\n\
         [keybindings.prefix-table]\nr = { action = \"enter-mode\", name = \"pick\", once = true }\n".to_owned(),
        // Parameterized-action arguments are the dispatcher's business.
        "[keybindings.prefix-table]\nR = { action = \"resize-pane\", direction = \"left\", amount = 3, made-up-arg = true }\n".to_owned(),
        "[[hooks.pane-exit]]\nwhen = { exit-code = 0 }\naction = \"noop\"\n\n\
         [[hooks.pane-exit]]\nwhen = { exit-code = \"*\" }\naction = { kind = \"run\", command = \"say done\" }\n\n\
         [[hooks.agent-state-changed]]\nwhen = { to = \"blocked\" }\naction = { kind = \"run\", command = [\"afplay\", \"/tmp/x.aiff\"] }\n".to_owned(),
        "[status]\nleft = [{ kind = \"windows\", separator = \" | \" }]\n\
         center = [\"help-hints\"]\n\
         right = [{ kind = \"session-name\", format = \"[{name}]\", style = { fg = \"red\", bold = true } }, { kind = \"time\", format = \" %H:%M\" }]\n".to_owned(),
        // Every colour syntax the renderer reads. A `name` or `file` that does
        // not resolve is not clean; those selectors have their own tests.
        "[theme]\naccent = \"#1e66f5\"\n\
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
        // Key modes: an enter-mode naming no mode, or no mode at all; a bad
        // binding inside a mode; options for a mode that does not exist; a
        // mode name the resolver refuses; ambiguity inside a mode.
        (
            "[keybindings.prefix-table]\nr = { action = \"enter-mode\", name = \"resise\" }\n",
            "keybindings.prefix-table.r",
            Fault::UnknownName,
            &["unknown key mode `resise` (did you mean `resize`?)"],
        ),
        (
            "[keybindings.prefix-table]\nr = \"enter-mode\"\n",
            "keybindings.prefix-table.r",
            Fault::BadValue,
            &["enter-mode needs a mode"],
        ),
        (
            "[keybindings.modes.resize]\nx = \"kill-pain\"\n",
            "keybindings.modes.resize.x",
            Fault::UnknownName,
            &["`kill-pane`"],
        ),
        (
            "[keybindings.mode-options.nope]\nexclusive = true\n",
            "keybindings.mode-options.nope",
            Fault::UnknownName,
            &["[keybindings.modes.nope]"],
        ),
        (
            "[keybindings.mode-options.resize]\nexclusiv = true\n",
            "keybindings.mode-options.resize.exclusiv",
            Fault::UnknownKey,
            &["exclusive"],
        ),
        (
            "[keybindings.modes.Big]\nx = \"detach\"\n",
            "keybindings.modes.Big",
            Fault::BadValue,
            &["invalid key mode name `Big`"],
        ),
        (
            "[keybindings.modes.resize]\n\"g\" = \"detach\"\n\"g x\" = \"detach\"\n",
            "keybindings.modes.resize.\"g x\"",
            Fault::BadChord,
            &["ambiguous prefix", "mode:resize g x"],
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

/// Palette [`crate::theme::load_dir`] accepts. Bright colours and `muted`
/// are derived; this is the same set as the theme-pack example.
const LOADABLE_THEME: &str = r##"
mode = "dark"
accent = "#bef264"
selection = "#293628"
background = "#0f1218"
foreground = "#c9d1dc"
red = "#f87171"
yellow = "#fde047"
green = "#86efac"
cyan = "#67e8f9"
blue = "#7aa2f7"
magenta = "#c4b5fd"
"##;

fn check_themes(input: &str, installed: &Path) -> CheckReport {
    check_with_themes(input, Path::new(PATH), Some(installed)).expect("check runs")
}

fn one(report: &CheckReport) -> &Finding {
    assert_eq!(report.findings.len(), 1, "{:?}", report.findings);
    &report.findings[0]
}

fn install_theme(root: &Path, name: &str, colors: &str) -> std::path::PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).expect("theme dir");
    std::fs::write(dir.join("colors.toml"), colors).expect("colors.toml");
    dir
}

/// Forward slashes, so a Windows temp path is not a TOML escape.
fn toml_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// `name = "nordd"` is the reload error, not a clean report.
#[test]
fn an_unknown_theme_name_is_the_reload_error() {
    let installed = tempfile::tempdir().expect("tempdir");
    let report = check_themes("[theme]\nname = \"nordd\"\n", installed.path());
    let found = one(&report);
    assert_eq!(found.path, "theme.name");
    assert_eq!(found.fault, Fault::UnknownName);
    assert_eq!(
        found.message,
        "no theme named \"nordd\"; `phux theme list` shows what is installed"
    );
    assert_eq!(found.source, Some(LayerSource::User(PATH.into())));
}

/// A name outside the catalog charset is the invalid-name error, not a miss.
#[test]
fn an_invalid_theme_name_is_refused() {
    let installed = tempfile::tempdir().expect("tempdir");
    let report = check_themes("[theme]\nname = \"Nord\"\n", installed.path());
    let found = one(&report);
    assert_eq!(found.path, "theme.name");
    assert_eq!(found.fault, Fault::UnknownName);
    assert_eq!(
        found.message,
        "\"Nord\" is not a usable theme name (lowercase letters, digits, `.`, `_`, `+`, `-`)"
    );
}

/// A name that [`crate::theme::load_dir`] can load, plus slot overrides, is clean.
#[test]
fn an_installed_theme_name_and_its_slot_overrides_check_clean() {
    let installed = tempfile::tempdir().expect("tempdir");
    install_theme(installed.path(), "phux-check-seam", LOADABLE_THEME);
    let report = check_themes(
        "[theme]\nname = \"phux-check-seam\"\naccent = \"#1e66f5\"\nchord = \"light-green\"\n",
        installed.path(),
    );
    assert!(report.is_ok(), "{:?}", report.findings);
}

/// The name is in the catalog, but its `colors.toml` does not load.
#[test]
fn a_named_theme_whose_palette_does_not_load_is_a_bad_value() {
    let installed = tempfile::tempdir().expect("tempdir");
    let dir = install_theme(installed.path(), "phux-check-seam", "not toml\n");
    let report = check_themes("[theme]\nname = \"phux-check-seam\"\n", installed.path());
    let found = one(&report);
    assert_eq!(found.path, "theme.name");
    assert_eq!(found.fault, Fault::BadValue);
    let expected = crate::theme::load_dir(&dir).expect_err("palette does not load");
    assert_eq!(found.message, expected.to_string());
}

/// `file` wins over `name`. A file that does not load is one bad value, and
/// the name beside it is not also required to exist.
#[test]
fn a_theme_file_that_does_not_load_is_refused_without_also_requiring_the_name() {
    let root = tempfile::tempdir().expect("tempdir");
    let missing = root.path().join("missing.toml");
    let broken = root.path().join("broken.toml");
    std::fs::write(&broken, "not = [\n").expect("write");
    let incomplete = root.path().join("incomplete.toml");
    std::fs::write(&incomplete, "mode = \"dark\"\nbackground = \"#000000\"\n").expect("write");
    for path in [&missing, &broken, &incomplete] {
        let shown = toml_path(path);
        let input = format!("[theme]\nfile = \"{shown}\"\nname = \"nordd\"\n");
        let report = check_themes(&input, root.path());
        let found = one(&report);
        assert_eq!(found.path, "theme.file", "{shown}");
        assert_eq!(found.fault, Fault::BadValue, "{shown}");
        let expected = crate::theme::load_file(Path::new(&shown)).expect_err("does not load");
        assert_eq!(found.message, expected.to_string(), "{shown}");
    }
}

/// A file that loads is clean even when `name` would not resolve.
#[test]
fn a_loadable_theme_file_checks_clean_even_when_the_name_does_not() {
    let root = tempfile::tempdir().expect("tempdir");
    let file = root.path().join("colors.toml");
    std::fs::write(&file, LOADABLE_THEME).expect("write");
    let shown = toml_path(&file);
    let input =
        format!("[theme]\nfile = \"{shown}\"\nname = \"Not A Theme\"\naccent = \"#1e66f5\"\n");
    let report = check_themes(&input, root.path());
    assert!(report.is_ok(), "{:?}", report.findings);
}

/// Slot findings stay, and they are not dropped when the selector is also wrong.
#[test]
fn a_slot_mistake_is_still_reported_beside_an_unknown_theme_name() {
    let installed = tempfile::tempdir().expect("tempdir");
    let report = check_themes(
        "[theme]\nacent = \"#123456\"\nname = \"nordd\"\n",
        installed.path(),
    );
    assert_eq!(paths(&report), vec!["theme.acent", "theme.name"]);
    assert_eq!(report.findings[0].fault, Fault::UnknownName);
    assert_eq!(report.findings[1].fault, Fault::UnknownName);
    assert!(
        report.findings[1]
            .message
            .contains("no theme named \"nordd\"")
    );
}

/// An enabled plugin theme resolves. A disabled plugin, and an empty plugin
/// list, do not, even when the manifest is on disk beside the config.
#[test]
fn plugin_themes_resolve_only_from_enabled_manifests() {
    let root = tempfile::tempdir().expect("tempdir");
    let plugin = root.path().join("plugin");
    install_theme(&plugin.join("themes"), "pack", LOADABLE_THEME);
    std::fs::write(
        plugin.join("phux-plugin.toml"),
        "id = \"test.theme\"\n\
         name = \"Test\"\n\
         version = \"0.1.0\"\n\
         min_phux_version = \"0.0.2\"\n\
         \n\
         [[themes]]\n\
         name = \"pack\"\n\
         path = \"themes/pack\"\n",
    )
    .expect("manifest");
    let config = plugin.join("config.toml");
    let installed = root.path().join("installed");
    std::fs::create_dir(&installed).expect("installed dir");

    let enabled = "[[plugins]]\nmanifest = \"phux-plugin.toml\"\n\n[theme]\nname = \"pack\"\n";
    let report = check_with_themes(enabled, &config, Some(&installed)).expect("check runs");
    assert!(
        report.is_ok(),
        "enabled plugin theme: {:?}",
        report.findings
    );

    let disabled = "[[plugins]]\nmanifest = \"phux-plugin.toml\"\nenabled = false\n\n\
                    [theme]\nname = \"pack\"\n";
    let report = check_with_themes(disabled, &config, Some(&installed)).expect("check runs");
    let found = one(&report);
    assert_eq!(found.path, "theme.name");
    assert_eq!(found.fault, Fault::UnknownName);
    assert!(found.message.contains("no theme named \"pack\""));

    // The manifest is on disk, but an empty plugin list does not search for it.
    let report = check_with_themes("[theme]\nname = \"pack\"\n", &config, Some(&installed))
        .expect("check runs");
    let found = one(&report);
    assert_eq!(found.path, "theme.name");
    assert!(found.message.contains("no theme named \"pack\""));
}

/// A manifest that fails to load is not a new finding, and it does not
/// suppress the installed catalog.
#[test]
fn a_broken_plugin_manifest_does_not_hide_the_installed_catalog() {
    let root = tempfile::tempdir().expect("tempdir");
    let installed = root.path().join("installed");
    install_theme(&installed, "phux-check-seam", LOADABLE_THEME);
    let config = root.path().join("config.toml");

    let unknown = "[[plugins]]\nmanifest = \"missing.toml\"\n\n[theme]\nname = \"nordd\"\n";
    let report = check_with_themes(unknown, &config, Some(&installed)).expect("check runs");
    let found = one(&report);
    assert_eq!(found.path, "theme.name");
    assert_eq!(found.fault, Fault::UnknownName);
    assert!(found.message.contains("no theme named \"nordd\""));

    let known = "[[plugins]]\nmanifest = \"missing.toml\"\n\n[theme]\nname = \"phux-check-seam\"\n";
    let report = check_with_themes(known, &config, Some(&installed)).expect("check runs");
    assert!(
        report.is_ok(),
        "installed name still resolves: {:?}",
        report.findings
    );
}
