//! Test-only guards on the generated CLI help.
//!
//! The rendered help itself is pinned byte-for-byte by the generated
//! reference docs; these tests lint what a diff would not catch: the root
//! page's grouping and width, internal ids leaking into `--help` (an
//! installed binary's user has no checkout), and the compiled agent skill
//! drifting from the selector grammar.

use crate::Cli;

/// Concatenate the long help of every command in the tree (root + all
/// subcommands), plain text, so id leaks anywhere in the surface are visible
/// to a single scan.
fn all_long_help(meta: &usage::spec::CommandMeta<'_>, buf: &mut String) {
    if let Some(page) = Cli::render_help(meta.cmd, true) {
        buf.push_str(&page);
        buf.push('\n');
    }
    for sub in meta.subcommands {
        if sub.cmd.name == "help" {
            continue;
        }
        all_long_help(sub, buf);
    }
}

/// Find `phux-<slug>` tokens whose slug looks like an internal ticket id.
/// Legitimate product tokens that share the `phux-` prefix (the `phux-ask`
/// title sentinel, a `phux-plugin.toml` manifest filename, crate names) are
/// allowlisted by their leading word; anything else — `phux-y8v6`,
/// `phux-foz.5`, `phux-l5xa` — is flagged.
fn ticket_like_tokens(help: &str) -> Vec<String> {
    const ALLOW: &[&str] = &[
        "plugin", "server", "web", "ask", "config", "core", "client", "protocol", "mcp",
    ];
    const NEEDLE: &str = "phux-";
    let mut hits = Vec::new();
    let mut cursor = 0;
    while let Some(rel) = help[cursor..].find(NEEDLE) {
        let slug_start = cursor + rel + NEEDLE.len();
        let slug: String = help[slug_start..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '.')
            .collect();
        if !slug.is_empty() && !ALLOW.iter().any(|word| slug.starts_with(word)) {
            hits.push(format!("phux-{slug}"));
        }
        cursor = slug_start;
    }
    hits
}

fn root_long_help() -> String {
    crate::render_help_page(Cli::command(), true, usage::help::Style::PLAIN).unwrap_or_default()
}

/// The groups the root inventory is laid out in, in page order. Every
/// visible verb declares exactly one of them; the renderer prints the
/// groups in this order because the verbs' `extra.display_order` values are
/// numbered by group.
const GROUPS: &[&str] = &[
    "Sessions", "Panes", "Agents", "Machines", "Maintain", "More",
];

#[test]
fn every_visible_verb_declares_one_of_the_root_groups() {
    for sub in Cli::spec().root.subcommands {
        if sub.hide {
            continue;
        }
        let heading = sub
            .extra
            .help_heading
            .unwrap_or_else(|| panic!("`phux {}` declares no help_heading", sub.cmd.name));
        assert!(
            GROUPS.contains(&heading),
            "`phux {}` is filed under unknown group {heading:?}",
            sub.cmd.name
        );
        assert!(
            sub.extra.display_order.is_some(),
            "`phux {}` declares no display_order; the groups would interleave",
            sub.cmd.name
        );
    }
}

#[test]
fn long_help_has_one_complete_grouped_inventory() {
    let long = root_long_help();
    let mut listed = Vec::new();
    let mut headings_seen = Vec::new();
    let mut in_group = false;
    for line in long.lines() {
        if let Some(title) = line.strip_suffix(':')
            && GROUPS.contains(&title)
        {
            headings_seen.push(title.to_owned());
            in_group = true;
            continue;
        }
        if in_group && line.trim().is_empty() {
            in_group = false;
            continue;
        }
        if in_group && let Some(name) = line.split_whitespace().next() {
            listed.push(name.to_owned());
        }
    }
    assert_eq!(
        headings_seen, GROUPS,
        "root groups are missing, duplicated, or out of order"
    );

    let mut expected: Vec<_> = Cli::spec()
        .root
        .subcommands
        .iter()
        .filter(|sub| sub.cmd.name != "help" && !sub.hide)
        .map(|sub| sub.cmd.name.to_owned())
        .collect();
    expected.sort();
    let mut sorted = listed.clone();
    sorted.sort();
    assert_eq!(
        sorted, expected,
        "grouped root inventory is incomplete or duplicated"
    );
    assert!(
        !long.contains("\nCommands:\n"),
        "an ungrouped `Commands:` section crept back into the root page"
    );
    for jargon in ["SPAWN_RESOURCE", "phux.agent/v1", " L3 "] {
        assert!(
            !long.contains(jargon),
            "root help leaks protocol jargon {jargon}"
        );
    }
}

/// The root page is a single readable screenful at 80 columns: no line
/// wider than the width it is laid out for, and the page as a whole stays
/// under the bound. Fifty-two visible verbs on their own rows (the three
/// approval verbs of ADR-0128 among them) plus six group headings set the
/// floor; the bound leaves no room for a reference-style epilogue to creep
/// back in.
#[test]
fn root_long_help_fits_eighty_columns_and_stays_short() {
    let long = root_long_help();
    for line in long.lines() {
        assert!(
            line.chars().count() <= 80,
            "root help line wider than 80 columns: {line:?}"
        );
    }
    let lines = long.lines().count();
    assert!(
        lines <= 93,
        "root help grew to {lines} lines; the page is meant to be one screen"
    );
    assert!(
        !long.contains("ENVIRONMENT") && !long.contains("EXIT STATUS"),
        "the reference epilogue belongs to `phux help environment` / `phux help exit-codes`"
    );
    let footer = long
        .find("Learn more:")
        .expect("root help ends with the Learn more footer");
    let tail = &long[footer..];
    for pointer in [
        "phux help targets",
        "phux help environment",
        "phux help exit-codes",
    ] {
        assert!(tail.contains(pointer), "Learn more footer lost {pointer}");
    }
}

/// Every verb's one-line summary fits its column: at most 58 characters
/// so the widest verb name still leaves the row on one line at 80 columns,
/// and no trailing period, so the inventory reads as one table.
#[test]
fn verb_summaries_are_short_and_unpunctuated() {
    for sub in Cli::spec().root.subcommands {
        if sub.hide {
            continue;
        }
        let about = sub.about.unwrap_or_default();
        let summary = about.split("\n\n").next().unwrap_or(about).trim();
        assert!(
            summary.chars().count() <= 58,
            "`phux {}` summary is {} chars: {summary:?}",
            sub.cmd.name,
            summary.chars().count()
        );
        assert!(
            !summary.ends_with('.'),
            "`phux {}` summary ends with a period: {summary:?}",
            sub.cmd.name
        );
    }
}

#[test]
fn help_leaks_no_internal_ids() {
    let mut buf = String::new();
    all_long_help(Cli::spec().root, &mut buf);
    // The stderr banner is user-facing too; scan it with the help strings.
    buf.push_str(crate::BANNER);
    buf.push('\n');

    assert!(
        !buf.contains("ADR-"),
        "user-facing help leaks an ADR reference; keep ADR ids in code \
         comments and docs, not help strings"
    );
    assert!(
        !buf.contains("docs/"),
        "user-facing help cites a repo-internal docs/ path; an installed \
         binary's user has no checkout — point at `phux help <verb>` or the \
         website instead"
    );
    assert!(
        !buf.contains("CREATE_SESSION"),
        "help still describes the removed CREATE_SESSION verb"
    );
    for jargon in ["SPAWN_RESOURCE", "phux.agent/v1", " L3 "] {
        assert!(
            !buf.contains(jargon),
            "user-facing help leaks protocol jargon {jargon}"
        );
    }
    let leaks = ticket_like_tokens(&buf);
    assert!(
        leaks.is_empty(),
        "user-facing help leaks internal ticket id(s): {leaks:?}"
    );
}

/// Every `phux agent` subcommand and every one of its args carries help
/// text visible in `--help` — no bare `target:`/`json:` fields whose
/// meaning the operator has to guess.
#[test]
fn agent_args_all_carry_doc_comments() {
    let root = Cli::spec().root;
    let agent = root
        .subcommands
        .iter()
        .copied()
        .find(|sub| sub.cmd.name == "agent")
        .expect("no `agent` subcommand in the tree");
    for sub in agent.subcommands {
        if sub.cmd.name == "help" {
            continue;
        }
        assert!(
            sub.about.is_some(),
            "`phux agent {}` has no about/doc comment",
            sub.cmd.name
        );
        for flag in sub.flags {
            if flag
                .flag
                .longs
                .iter()
                .any(|name| matches!(*name, "help" | "version"))
            {
                continue;
            }
            assert!(
                flag.help.is_some(),
                "`phux agent {}` flag `{}` carries no doc comment visible \
                 in --help",
                sub.cmd.name,
                flag.flag.longs.first().copied().unwrap_or("?")
            );
        }
    }
}

/// The exit-status table left the root page for a topic; the topic must
/// render the canonical table, and the root page must point at it.
#[test]
fn help_topics_render_their_sources() {
    let exit = crate::help_topic("exit-codes").expect("exit-codes topic");
    assert_eq!(exit.trim_end(), crate::exit_codes::exit_status_section());
    for code in ["124", "125"] {
        assert!(
            exit.lines().any(|line| line.trim_start().starts_with(code)),
            "`phux help exit-codes` no longer documents {code}"
        );
    }
    for alias in ["exit-status", "exit"] {
        assert_eq!(crate::help_topic(alias), Some(exit.clone()));
    }

    let env = crate::help_topic("environment").expect("environment topic");
    assert_eq!(env, crate::environment::environment_section());
    assert_eq!(crate::help_topic("env"), Some(env));

    let targets = crate::help_topic("targets").expect("targets topic");
    for sigil in [
        "name:W.P",
        "@N",
        "#tag",
        "%agent",
        "host/@N",
        "`=` is reserved",
    ] {
        assert!(targets.contains(sigil), "targets topic lost {sigil}");
    }
    assert_eq!(crate::help_topic("selectors"), Some(targets));

    assert_eq!(crate::help_topic("attach"), None, "a verb is not a topic");
    assert_eq!(crate::help_topic(""), None);

    assert!(root_long_help().contains("phux help exit-codes"));
}

/// The remedy every skill-drift failure names.
const SKILL_REMEDY: &str = "document it in .agents/skills/using-phux/SKILL.md (the file \
     `phux skill` prints, compiled into the binary by include_str!)";

/// The token the skill must use to teach the selector form `selector` is.
///
/// Deliberately an exhaustive match with **no wildcard arm**: adding a variant
/// to `Selector` (a new sigil) fails to compile here, which is the point — a
/// grammar the skill does not teach is a grammar an agent cannot type. Keep
/// the tokens as they appear in the skill's selector table.
fn taught_selector_token(selector: &crate::selector::Selector) -> &'static str {
    use crate::selector::Selector;

    match selector {
        Selector::Current => "`.`",
        Selector::Session(_) => "`name`",
        Selector::Window(..) => "`name:W`",
        Selector::Pane(..) => "`name:W.P`",
        Selector::ResourceId(_) => "@N",
        Selector::SatelliteResourceId { .. } => "host/@N",
        Selector::Tag(_) => "#tag",
        Selector::Agent(_) => "%name",
    }
}

/// Every selector form the parser accepts is taught in the compiled skill,
/// and the one form it deliberately refuses is explained rather than omitted.
///
/// The probes go through the real parser, so a sigil that is added to the
/// grammar starts failing this test the moment it parses — no second list to
/// keep in step. `=` parses to an error on purpose (it means the attached
/// TUI's focus history, which a headless caller does not have); an agent that
/// meets that refusal with no explanation retries it, so the skill must name
/// it too.
#[test]
fn skill_teaches_every_selector_sigil_the_parser_accepts() {
    let skill = crate::skill::render(crate::skill::SkillScope::Quick);
    for probe in [
        "@7",
        "edge/@7",
        ".",
        "work",
        "work:1",
        "work:1.0",
        "#build",
        "%reviewer",
    ] {
        let Ok(selector) = crate::selector::parse(probe) else {
            continue;
        };
        let token = taught_selector_token(&selector);
        assert!(
            skill.contains(token),
            "the parser accepts the selector `{probe}` but the compiled agent \
             skill never teaches {token}; {SKILL_REMEDY}"
        );
    }

    assert!(
        crate::selector::parse("=").is_err(),
        "`=` is refused for headless callers; if that changed, teach it"
    );
    assert!(
        skill.contains("`=`"),
        "the compiled agent skill must explain why `=` is refused; {SKILL_REMEDY}"
    );
}

/// The load-bearing rules the skill exists to carry, pinned by name.
///
/// Each of these is a sentence an orchestrating agent gets wrong without it,
/// and each was a named gap before the skill was compiled in:
/// the am-I-inside-phux check (phux injects both variables into every pane it
/// spawns, and the skill never said so, so an agent could not avoid prompting
/// itself); the level-versus-edge distinction (a level read of `idle` is
/// equally true of a crashed pane, so a completion gate MUST require an
/// observed transition); and the two timeout codes, which mean different
/// things and are routinely conflated.
#[test]
fn skill_teaches_the_load_bearing_rules() {
    let skill = crate::skill::render(crate::skill::SkillScope::Quick);
    for needle in [
        "PHUX_TERMINAL_ID",
        "PHUX_SOCKET",
        "observed transition",
        "level read",
        "124",
        "125",
    ] {
        assert!(
            skill.contains(needle),
            "the compiled agent skill no longer teaches {needle:?}; it is one \
             of the rules the skill exists to carry"
        );
    }
}

/// The skill is read by agents driving an INSTALLED binary, so it may not
/// cite anything only a checkout has — the same rule `help_leaks_no_internal_ids`
/// applies to `--help`. Repo paths, ADR numbers, and bead ids all belong in
/// the source and the docs tree, not in the text a stranger's `phux skill`
/// prints.
#[test]
fn skill_cites_nothing_only_a_checkout_has() {
    let skill = crate::skill::render(crate::skill::SkillScope::Full);
    assert!(
        !skill.contains("ADR-"),
        "the compiled agent skill cites an ADR; its reader has no checkout"
    );
    assert!(
        !skill.contains("docs/"),
        "the compiled agent skill cites a repo-internal docs/ path; point at \
         `phux help <verb>` instead"
    );
    let leaks = ticket_like_tokens(&skill);
    assert!(
        leaks.is_empty(),
        "the compiled agent skill leaks internal ticket id(s): {leaks:?}"
    );
}
