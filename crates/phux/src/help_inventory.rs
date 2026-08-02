//! Test-only guards on the generated CLI help.
//!
//! Two properties are pinned here so they fail CI on drift:
//!
//! 1. The full command inventory (every `phux …` invocation path) matches a
//!    checked-in snapshot, so a newly-wired or removed subcommand forces this
//!    file — and whoever adds the command — to acknowledge the surface change.
//! 2. No user-facing help string leaks an internal ticket id (`phux-xxxx`) or
//!    ADR reference (`ADR-00xx`), and none still describes the removed
//!    `CREATE_SESSION` verb. Those belong in code comments and `docs/`, never
//!    in `--help`.

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
        if name == "help" {
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

    assert!(
        !buf.contains("ADR-"),
        "user-facing help leaks an ADR reference; keep ADR ids in code \
         comments and docs, not help strings"
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
