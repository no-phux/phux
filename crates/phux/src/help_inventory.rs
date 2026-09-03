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
//! 4. The compiled agent skill (`crate::SKILL`, printed by `phux skill`)
//!    mentions every visible top-level verb, every `phux agent` subcommand,
//!    and every selector sigil the parser accepts. That is the anti-drift
//!    gate the skill exists for: it is compiled in so it cannot lag the
//!    binary, and these tests are what make "it mentions the surface" true
//!    by CI rather than by memory. The skill had already drifted before it
//!    was compiled in — `agent wait` and `agent send-keys` shipped and the
//!    example copy never learned they existed.

use clap::CommandFactory;

use crate::Cli;

/// Recursively collect every command invocation path (`phux`, `phux agent`,
/// `phux agent set`, …), skipping clap's auto-injected `help` pseudo-command.
fn collect_paths(cmd: &clap::Command, prefix: &str, out: &mut Vec<String>) {
    out.push(prefix.to_owned());
    for sub in cmd.get_subcommands() {
        if sub.get_name() == "help" {
            continue;
        }
        let child = format!("{prefix} {}", sub.get_name());
        collect_paths(sub, &child, out);
    }
}

/// The sorted inventory of command paths as one path per line.
fn command_inventory() -> String {
    let root = Cli::command();
    let mut paths = Vec::new();
    collect_paths(&root, "phux", &mut paths);
    paths.sort();
    paths.join("\n")
}

/// Concatenate the long help of every command in the tree (root + all
/// subcommands), plain text, so id leaks anywhere in the surface are visible
/// to a single scan.
fn all_long_help(cmd: &clap::Command, buf: &mut String) {
    let mut owned = cmd.clone();
    buf.push_str(&owned.render_long_help().to_string());
    buf.push('\n');
    for sub in cmd.get_subcommands() {
        if sub.get_name() == "help" {
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
        "plugin", "server", "web", "ask", "config", "core", "client", "protocol",
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
phux agent explain
phux agent install-claude
phux agent list
phux agent prompt
phux agent report-state
phux agent send-keys
phux agent set
phux agent show
phux agent start
phux agent uninstall-claude
phux agent wait
phux ask
phux attach
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
phux resize
phux run
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

#[test]
fn top_level_help_lists_every_subcommand() {
    let mut root = Cli::command();
    let long = root.render_long_help().to_string();
    for sub in Cli::command().get_subcommands() {
        let name = sub.get_name();
        // Hidden subcommands (internal tooling like `gen-reference-docs`)
        // are deliberately absent from the curated help.
        if name == "help" || sub.is_hide_set() {
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
    assert!(crate::SHORT_HELP.contains("phux --help` for every command"));
    let daily = crate::SHORT_HELP
        .lines()
        .filter(|line| line.trim_start().starts_with("phux "))
        .count();
    assert!(
        (6..=8).contains(&daily),
        "short help should carry 6-8 daily commands, got {daily}:\n{}",
        crate::SHORT_HELP
    );
    assert!(!crate::SHORT_HELP.contains("ATTACH / SERVE"));
}

#[test]
fn long_help_has_one_complete_grouped_inventory() {
    const HEADINGS: &[&str] = &[
        "ATTACH / SERVE",
        "INSPECT",
        "DRIVE",
        "SUPERVISE",
        "ORGANIZE",
        "FEDERATION",
    ];
    let mut root = Cli::command();
    let long = root.render_long_help().to_string();
    let mut listed = Vec::new();
    let mut in_group = false;
    for line in long.lines() {
        let trimmed = line.trim();
        if HEADINGS.contains(&trimmed) {
            in_group = true;
            continue;
        }
        if in_group && trimmed.is_empty() {
            in_group = false;
            continue;
        }
        if in_group && let Some(name) = trimmed.split_whitespace().next() {
            listed.push(name.to_owned());
        }
    }

    let mut expected: Vec<_> = Cli::command()
        .get_subcommands()
        .filter(|sub| sub.get_name() != "help" && !sub.is_hide_set())
        .map(|sub| sub.get_name().to_owned())
        .collect();
    expected.sort();
    listed.sort();
    assert_eq!(
        listed, expected,
        "grouped root inventory is incomplete or duplicated"
    );
    assert!(
        !long.contains("\nCommands:\n"),
        "flat Clap catalog returned"
    );
    for jargon in ["SPAWN_TERMINAL", "phux.agent/v1", " L3 "] {
        assert!(
            !long.contains(jargon),
            "root help leaks protocol jargon {jargon}"
        );
    }
}

#[test]
fn help_leaks_no_internal_ids() {
    let mut buf = String::new();
    all_long_help(&Cli::command(), &mut buf);
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
    for jargon in ["SPAWN_TERMINAL", "phux.agent/v1", " L3 "] {
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
            "phux completion zsh  > ~/.zfunc/_phux   (~/.zfunc must be on $fpath)",
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
];

#[test]
fn example_blocks_render_one_example_per_line() {
    let root = Cli::command();
    for (name, examples) in EXAMPLE_BLOCKS {
        let sub = root
            .get_subcommands()
            .find(|sub| sub.get_name() == *name)
            .unwrap_or_else(|| panic!("no `{name}` subcommand in the tree"));
        let long = sub.clone().render_long_help().to_string();
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
    use clap::Parser as _;

    let mut root = Cli::command();
    let long = root.render_long_help().to_string();
    assert!(
        !long.contains("stdio-bridge"),
        "top-level `phux --help` still advertises the machine-only \
         `stdio-bridge`:\n{long}"
    );

    assert!(
        Cli::try_parse_from(["phux", "stdio-bridge"]).is_ok(),
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
    let root = Cli::command();
    let attach = root
        .get_subcommands()
        .find(|sub| sub.get_name() == "attach")
        .expect("no `attach` subcommand in the tree");
    let long = attach.clone().render_long_help().to_string();
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
    let root = Cli::command();
    let agent = root
        .get_subcommands()
        .find(|sub| sub.get_name() == "agent")
        .expect("no `agent` subcommand in the tree");
    for sub in agent.get_subcommands() {
        if sub.get_name() == "help" {
            continue;
        }
        assert!(
            sub.get_about().is_some(),
            "`phux agent {}` has no about/doc comment",
            sub.get_name()
        );
        for arg in sub.get_arguments() {
            if matches!(arg.get_id().as_str(), "help" | "version") {
                continue;
            }
            assert!(
                arg.get_help().is_some(),
                "`phux agent {}` arg `{}` carries no doc comment visible \
                 in --help",
                sub.get_name(),
                arg.get_id()
            );
        }
    }
}

#[test]
fn root_help_documents_exit_status() {
    let mut root = Cli::command();
    let long = root.render_long_help().to_string();
    assert!(
        long.contains("EXIT STATUS"),
        "root --help lost its EXIT STATUS section"
    );
    for code in ["124", "125"] {
        assert!(
            long.lines().any(|line| line.trim_start().starts_with(code)),
            "root --help's EXIT STATUS no longer documents {code}"
        );
    }
}

#[test]
fn parser_reserved_agent_selector_is_not_advertised_as_live() {
    let mut root = Cli::command();
    let help = root.render_long_help().to_string();
    assert!(
        !help.contains("%agent-name"),
        "root --help advertises the parser-reserved `%name` form as live"
    );

    let skill = crate::skill::render(crate::skill::SkillScope::Full);
    assert!(
        skill.contains("no shipped verb resolves it"),
        "the compiled skill must explain that `%name` is parser-reserved"
    );
}

// ---------------------------------------------------------------------------
// The compiled agent skill vs the surface it describes
//
// `skill::SOURCE` is `include_str!`d, so it always belongs to this build — but
// "compiled in" only guarantees it ships together with the binary, not that it
// still says true things about it. These three tests are the part that catches
// drift, on the same principle as
// `refdocs::tests::generated_reference_docs_match_the_tree`: derive the
// expectation from the clap tree and the selector parser rather than from a
// second checked-in list.
// ---------------------------------------------------------------------------

/// The remedy every skill-drift failure names. One string so the three tests
/// cannot teach three different fixes.
const SKILL_REMEDY: &str = "document it in skills/phux/SKILL.md (the file \
     `phux skill` prints, compiled into the binary by include_str!)";

/// Every visible top-level verb is named in the compiled skill.
///
/// The skill is the agent-to-agent UX: it is what another agent reads to learn
/// what this CLI can do. A verb the skill never mentions is a verb no agent
/// will ever call, which is how `take`, `give`, `signal`, `rec`, `play`, and
/// `worktree` stayed invisible for releases at a time. Hidden verbs
/// (`gen-reference-docs`, `stdio-bridge`) are machine plumbing and are
/// deliberately excluded, exactly as they are from the curated `--help`.
#[test]
fn skill_names_every_visible_top_level_verb() {
    let skill = crate::skill::render(crate::skill::SkillScope::Full);
    for sub in Cli::command().get_subcommands() {
        let name = sub.get_name();
        if name == "help" || sub.is_hide_set() {
            continue;
        }
        let needle = format!("phux {name}");
        assert!(
            skill.contains(&needle),
            "the compiled agent skill never mentions `{needle}`; {SKILL_REMEDY}"
        );
    }
}

/// Every visible `phux agent` subcommand is named in the compiled skill.
///
/// The `agent` namespace is the skill's whole subject, so this is where drift
/// costs the most: `agent wait` and `agent send-keys` both shipped while the
/// hand-maintained example skill still described a surface without them.
#[test]
fn skill_names_every_agent_subcommand() {
    let skill = crate::skill::render(crate::skill::SkillScope::Agent);
    let root = Cli::command();
    let agent = root
        .get_subcommands()
        .find(|sub| sub.get_name() == "agent")
        .expect("no `agent` subcommand in the tree");
    for sub in agent.get_subcommands() {
        let name = sub.get_name();
        if name == "help" || sub.is_hide_set() {
            continue;
        }
        let needle = format!("agent {name}");
        assert!(
            skill.contains(&needle),
            "the compiled agent skill never mentions `phux {needle}`; {SKILL_REMEDY}"
        );
    }
}

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
        Selector::TerminalId(_) => "@N",
        Selector::SatelliteTerminalId { .. } => "host/@N",
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
