//! Test-only guards on the generated CLI help.
//!
//! Three properties are pinned here so they fail CI on drift:
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
phux agent clear
phux agent explain
phux agent install-claude
phux agent list
phux agent set
phux agent show
phux agent uninstall-claude
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
phux enroll
phux gen-reference-docs
phux give
phux insert-pane
phux kill
phux launch
phux logs
phux ls
phux move-pane
phux new
phux pair
phux paste
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
phux remote
phux remote add
phux remote list
phux remote remove
phux rename
phux resize
phux run
phux satellite
phux satellite add
phux satellite enroll
phux satellite list
phux satellite remove
phux send-keys
phux server
phux service
phux service install
phux service logs
phux service prune-logs
phux service status
phux service uninstall
phux signal
phux snapshot
phux spawn
phux stdio-bridge
phux swap-pane
phux tag
phux tag add
phux tag ls
phux tag rm
phux take
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
         src/help_inventory.rs and the curated top-level help in main.rs"
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
