//! Test-only guards on the generated CLI help.
//!
//! Four properties are pinned here so they fail CI on drift:
//!
//! 1. The full command inventory (every `phux …` invocation path) matches a
//!    checked-in snapshot, so a newly-wired or removed subcommand forces this
//!    file — and whoever adds the command — to acknowledge the surface change.
//! 2. No user-facing help string (nor the stderr banner) leaks an internal
//!    ticket id (`phux-xxxx`), an ADR reference (`ADR-00xx`), or a
//!    repo-internal `docs/` path, and none still describes the removed
//!    `CREATE_SESSION` verb. Those belong in code comments and the repo's
//!    docs, never in `--help` — an installed binary's user has no checkout.
//! 3. The verbs that carry worked examples render them one per line
//!    (`EXAMPLE_BLOCKS`), and the root help documents EXIT STATUS.
//! 4. The compiled agent skill (`crate::skill::SOURCE`, printed by `phux
//!    skill`) teaches every selector sigil and the small set of invariants an
//!    agent needs before consulting the generated `phux help` surface.

use crate::Cli;

/// Recursively collect every command invocation path (`phux`, `phux agent`,
/// `phux agent set`, …).
fn collect_paths(meta: &usage::spec::CommandMeta<'_>, prefix: &str, out: &mut Vec<String>) {
    out.push(prefix.to_owned());
    for sub in meta.subcommands {
        if sub.cmd.name == "help" {
            continue;
        }
        let child = format!("{prefix} {}", sub.cmd.name);
        collect_paths(sub, &child, out);
    }
}

/// The sorted inventory of command paths as one path per line.
fn command_inventory() -> String {
    let mut paths = Vec::new();
    collect_paths(Cli::spec().root, "phux", &mut paths);
    paths.sort();
    paths.join("\n")
}

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

/// The complete, sorted `phux` command inventory. A new subcommand (or a
/// removed one) must update this snapshot, which keeps the curated top-level
/// help and the docs honest about the shipped surface.
const EXPECTED_INVENTORY: &str = "\
phux
phux agent
phux agent answer
phux agent clear
phux agent emit
phux agent explain
phux agent hook-payload
phux agent install-claude
phux agent list
phux agent log
phux agent prompt
phux agent report-state
phux agent send-keys
phux agent session
phux agent session close
phux agent session open
phux agent set
phux agent show
phux agent start
phux agent uninstall-claude
phux agent wait
phux ask
phux attach
phux bootstrap
phux channel
phux cockpit
phux completion
phux config
phux config agents
phux config check
phux config init
phux config path
phux config plugins
phux config reload
phux config run
phux config show
phux detach
phux doctor
phux gen-reference-docs
phux give
phux host
phux host add
phux host enroll
phux host ls
phux host rm
phux insert-pane
phux kill
phux launch
phux logs
phux ls
phux mcp
phux move-pane
phux new
phux pair
phux pair revoke
phux pair rotate
phux paste
phux perf
phux play
phux plugin
phux plugin disable
phux plugin enable
phux plugin install
phux plugin link
phux plugin list
phux plugin unlink
phux plugin update
phux plugin validate
phux rec
phux relay
phux relay pair
phux relay run
phux rename
phux report
phux report new
phux report show
phux resize
phux run
phux runtime-info
phux send-keys
phux server
phux service
phux service install
phux service logs
phux service prune-logs
phux service reconcile
phux service status
phux service uninstall
phux signal
phux skill
phux snapshot
phux spawn
phux status
phux stdio-bridge
phux swap-pane
phux tag
phux tag add
phux tag ls
phux tag rm
phux take
phux update
phux upgrade
phux wait
phux watch
phux whoami
phux workspace
phux workspace inspect
phux workspace restore
phux workspace save
phux worktree
phux worktree list
phux worktree new
phux worktree open
phux worktree remove";

#[test]
fn command_inventory_matches_snapshot() {
    assert_eq!(
        command_inventory(),
        EXPECTED_INVENTORY,
        "the phux command inventory drifted from the pinned snapshot; if you \
         added or removed a subcommand, update EXPECTED_INVENTORY in \
         src/help_inventory.rs and the curated top-level help in lib.rs"
    );
}

fn root_long_help() -> String {
    crate::render_help_page(Cli::command(), true, usage::help::Style::PLAIN).unwrap_or_default()
}

#[test]
fn top_level_help_lists_every_subcommand() {
    let long = root_long_help();
    for sub in Cli::spec().root.subcommands {
        let name = sub.cmd.name;
        if name == "help" || sub.hide {
            continue;
        }
        assert!(
            long.contains(name),
            "top-level `phux --help` omits `{name}` from its curated inventory"
        );
    }
}

#[test]
fn short_help_is_a_small_start_here_view() {
    assert!(crate::SHORT_HELP.contains("phux                     Attach"));
    assert!(crate::SHORT_HELP.contains("phux host add me@HOST"));
    assert!(crate::SHORT_HELP.contains("phux --help` for every command"));
    let daily = crate::SHORT_HELP
        .lines()
        .filter(|line| line.trim_start().starts_with("phux "))
        .count();
    assert!(
        (6..=9).contains(&daily),
        "short help should carry 6-9 daily commands, got {daily}:\n{}",
        crate::SHORT_HELP
    );
    assert!(!crate::SHORT_HELP.contains("Sessions:"));
}

/// The groups the root inventory is laid out in, in page order. Every
/// visible verb declares exactly one of them; the renderer prints the
/// groups in this order because the verbs' `display_order` values are
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
            .help_heading
            .unwrap_or_else(|| panic!("`phux {}` declares no help_heading", sub.cmd.name));
        assert!(
            GROUPS.contains(&heading),
            "`phux {}` is filed under unknown group {heading:?}",
            sub.cmd.name
        );
        assert!(
            sub.display_order.is_some(),
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
/// under the bound. Forty-nine visible verbs on their own rows plus six
/// group headings set the floor; the bound leaves no room for a
/// reference-style epilogue to creep back in.
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
        lines <= 90,
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

/// Every command whose long help carries worked examples, with the exact
/// example lines it must render. Each example must appear on its own line:
/// clap reflows doc-comment paragraphs, so an example block written as a doc
/// comment collapses onto one run-on line — three shell commands run together
/// do copy-paste damage. The fix is a hand-written `long_about` with real
/// newlines (the `rec`/`play` pattern); this table keeps the eight converted
/// verbs from regressing.
const EXAMPLE_BLOCKS: &[(&str, &[&str])] = &[
    (
        "send-keys",
        &[
            "phux send-keys demo \"echo hi\" Enter",
            "phux send-keys work:1.0 C-c",
        ],
    ),
    (
        "wait",
        &[
            "phux wait --until \"BUILD SUCCESSFUL\" build",
            "phux wait --idle 750 repl",
        ],
    ),
    (
        "run",
        &[
            "phux run build \"cargo test\"",
            "phux run --timeout 30 work:1.0 \"cargo test\"",
        ],
    ),
    (
        "completion",
        &[
            "phux completion zsh > ~/.zfunc/_phux (~/.zfunc must be on $fpath)",
            "phux completion bash > ~/.local/share/bash-completion/completions/phux",
            "phux completion fish > ~/.config/fish/completions/phux.fish",
        ],
    ),
    (
        "resize",
        &["phux resize demo 120x40", "phux resize @7 200x50 --json"],
    ),
    (
        "signal",
        &["phux signal build freeze", "phux signal . kill"],
    ),
    (
        "paste",
        &[
            "phux paste demo 'SELECT count(*) FROM users;'",
            "git diff | phux paste review",
        ],
    ),
    (
        "ask",
        &[
            "phux ask work:1.0 --id deploy --suggest Yes --suggest No \"Deploy?\"",
            "phux ask @3 --json \"Need approval\"",
        ],
    ),
    (
        "channel",
        &["phux channel", "phux channel next", "phux channel latest"],
    ),
    ("cockpit", &["phux cockpit", "phux cockpit --json"]),
];

#[test]
fn example_blocks_render_one_example_per_line() {
    let root = Cli::spec().root;
    for (name, examples) in EXAMPLE_BLOCKS {
        let sub = root
            .subcommands
            .iter()
            .copied()
            .find(|sub| sub.cmd.name == *name)
            .unwrap_or_else(|| panic!("no `{name}` subcommand in the tree"));
        let long = Cli::render_help(sub.cmd, true).unwrap_or_default();
        assert!(
            long.contains("Examples:"),
            "`phux {name} --help` lost its Examples: block:\n{long}"
        );
        for example in *examples {
            assert!(
                long.lines().any(|line| line.trim() == *example),
                "`phux {name} --help` does not render {example:?} on its own \
                 line (clap reflowed it?):\n{long}"
            );
        }
    }
}

/// `stdio-bridge` is machine-only plumbing — the remote end of the
/// SSH-stdio transport that `ssh HOST phux stdio-bridge` invokes. No human
/// types it, so the curated help must not advertise it, while the verb
/// itself keeps parsing (hiding it must never break deployed `ssh` bridge
/// invocations).
#[test]
fn top_level_help_hides_stdio_bridge_but_it_still_parses() {
    let long = root_long_help();
    assert!(
        !long.contains("stdio-bridge"),
        "top-level `phux --help` still advertises the machine-only \
         `stdio-bridge`:\n{long}"
    );

    assert!(
        crate::parse_cli(["phux", "stdio-bridge"]).is_ok(),
        "`phux stdio-bridge` must keep parsing while hidden"
    );
}

/// `phux attach NAME` resolves NAME against the host registry first — an
/// enrolled host shadows a local session of the same name — and `--socket`
/// is the escape hatch that forces the local reading. Both halves of that
/// rule must be taught in the attach long help. Matched on
/// whitespace-normalized text because clap reflows doc-comment paragraphs.
#[test]
fn attach_long_help_documents_registry_shadowing_and_socket() {
    let root = Cli::spec().root;
    let attach = root
        .subcommands
        .iter()
        .copied()
        .find(|sub| sub.cmd.name == "attach")
        .expect("no `attach` subcommand in the tree");
    let long = Cli::render_help(attach.cmd, true).unwrap_or_default();
    let flat = long.split_whitespace().collect::<Vec<_>>().join(" ");
    for needle in [
        "phux host enroll",
        "shadows a local session",
        "--socket` to force the local reading",
    ] {
        assert!(
            flat.contains(needle),
            "`phux attach --help` no longer documents registry-name \
             shadowing ({needle:?} missing):\n{long}"
        );
    }
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

#[test]
fn the_agent_selector_is_advertised_as_live() {
    // `%name` has its production caller (ADR-0075 via ADR-0103): the shared
    // target resolver and the agent-session verbs branch on it, so the root
    // help and the compiled skill teach it as a live form.
    let targets = crate::help_topic("targets").expect("targets topic");
    assert!(
        targets.contains("%agent"),
        "`phux help targets` must advertise the `%name` form now that verbs resolve it"
    );

    let skill = crate::skill::render(crate::skill::SkillScope::Full);
    assert!(
        !skill.contains("no shipped verb resolves it"),
        "the compiled skill must not call `%name` parser-reserved any more"
    );
}

// ---------------------------------------------------------------------------
// The compiled agent skill vs the surface it describes
//
// `skill::SOURCE` is `include_str!`d, so it always belongs to this build — but
// "compiled in" only guarantees it ships together with the binary, not that it
// still says true things about it. These tests catch drift, on the same
// principle as
// `refdocs::tests::generated_reference_docs_match_the_tree`: derive the
// expectation from the usage spec and the selector parser rather than from a
// second checked-in list.
// ---------------------------------------------------------------------------

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
