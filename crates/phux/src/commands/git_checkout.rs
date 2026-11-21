//! `phux git status|diff|log|branches|pr|show TARGET`: one `CHECKOUT` read
//! on the checkout a pane is in, printed as the typed answer.

use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::selector::format_terminal_id;
use phux_protocol::checkout::{CheckoutAnswer, CheckoutArea, CheckoutRequest};
use phux_protocol::ids::ResourceId;
use serde::Serialize;

use crate::commands::ctx;
use crate::commands::{GitAction, GitArgs, parse_target, resolve_target};

/// Run `phux git` as parsed.
pub(crate) fn run(args: GitArgs, socket: Option<PathBuf>) -> ExitCode {
    let (target, request, json) = request_of(args.action);
    ctx::run_local(socket, json, |ctx| {
        let selector = parse_target(Some(&target))?;
        ctx.rt.block_on(async {
            let pane = resolve_target(ctx.socket(), &selector, "git").await?;
            let answer = phux_client::checkout::checkout(ctx.socket(), &pane, request)
                .await
                .map_err(|err| ctx.unreachable(&err, "git"))?;
            Ok(report(&pane, &answer, json))
        })
    })
}

/// The target, the wire request, and `--json` of one parsed verb.
fn request_of(action: GitAction) -> (String, CheckoutRequest, bool) {
    match action {
        GitAction::Status { target, json } => (target, CheckoutRequest::Status, json.json),
        GitAction::Diff {
            target,
            path,
            staged,
            untracked,
            json,
        } => {
            let area = if staged {
                CheckoutArea::Staged
            } else if untracked {
                CheckoutArea::Untracked
            } else {
                CheckoutArea::Unstaged
            };
            (target, CheckoutRequest::Diff { path, area }, json.json)
        }
        GitAction::Log {
            target,
            path,
            skip,
            limit,
            json,
        } => (
            target,
            CheckoutRequest::Log { skip, limit, path },
            json.json,
        ),
        GitAction::Branches { target, json } => (target, CheckoutRequest::Branches, json.json),
        GitAction::Pr { target, json } => (target, CheckoutRequest::PullRequest, json.json),
        GitAction::Show { target, sha, json } => (target, CheckoutRequest::Show { sha }, json.json),
    }
}

/// Print the answer and exit 1 on a typed refusal. Under `--json` the
/// refusal is the answer document on stdout, as `phux upload` prints a
/// refused receipt, so its typed code survives.
fn report(pane: &ResourceId, answer: &CheckoutAnswer, json: bool) -> ExitCode {
    let refused = matches!(answer, CheckoutAnswer::Refused(_));
    if json {
        outln!(
            "{}",
            serde_json::json!({
                "schema_version": 1,
                "terminal_id": format_terminal_id(pane),
                "answer": answer,
            })
        );
    } else {
        human(answer);
    }
    if refused {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn human(answer: &CheckoutAnswer) {
    match answer {
        CheckoutAnswer::Status(status) => {
            let branch = status.branch.as_deref().unwrap_or("(detached)");
            match &status.upstream {
                Some(up) => outln!(
                    "{branch}...{up} [ahead {}, behind {}]",
                    status.ahead,
                    status.behind
                ),
                None => outln!("{branch}"),
            }
            for entry in &status.entries {
                outln!(
                    "{:<10} {} {}{}",
                    word(&entry.area),
                    entry.code,
                    entry.path,
                    counts(entry.added, entry.deleted)
                );
            }
        }
        CheckoutAnswer::Diff(diff) => out!("{}", diff.text),
        CheckoutAnswer::Log(log) => {
            for commit in &log.commits {
                outln!("{} {}", commit.short_sha, commit.subject);
            }
        }
        CheckoutAnswer::Branches(branches) => {
            for branch in &branches.branches {
                let mark = if branch.current { '*' } else { ' ' };
                outln!("{mark} {} {} {}", branch.name, branch.sha, branch.subject);
            }
        }
        CheckoutAnswer::PullRequest(pr) => {
            outln!(
                "#{} {} [{}] {}",
                pr.number,
                pr.title,
                word(&pr.state),
                pr.url
            );
            for check in &pr.checks {
                outln!("  {:<10} {}", word(&check.state), check.name);
            }
        }
        CheckoutAnswer::Commit(detail) => {
            outln!(
                "commit {}\n\n{}\n",
                detail.commit.sha,
                detail.message.trim_end()
            );
            out!("{}", detail.diff);
        }
        CheckoutAnswer::Refused(refusal) | CheckoutAnswer::Applied(refusal) => {
            error!("git: {}: {}", word(&refusal.code), refusal.message);
        }
        other => outln!(
            "{}",
            serde_json::to_string_pretty(other).unwrap_or_default()
        ),
    }
}

/// A `snake_case` wire enum as its wire word.
fn word(value: &impl Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn counts(added: Option<u32>, deleted: Option<u32>) -> String {
    match (added, deleted) {
        (Some(added), Some(deleted)) => format!(" (+{added} -{deleted})"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "tests")]

    use super::*;

    /// Every verb shape maps to the request it promises.
    #[test]
    fn each_verb_maps_to_its_checkout_request() {
        let area = |area| CheckoutRequest::Diff {
            path: "src/a.rs".into(),
            area,
        };
        for (argv, want, want_json) in [
            ("status @1 --json", CheckoutRequest::Status, true),
            ("diff @1 src/a.rs", area(CheckoutArea::Unstaged), false),
            (
                "diff @1 src/a.rs --staged",
                area(CheckoutArea::Staged),
                false,
            ),
            (
                "diff @1 src/a.rs --untracked",
                area(CheckoutArea::Untracked),
                false,
            ),
            (
                "log @1",
                CheckoutRequest::Log {
                    skip: 0,
                    limit: 20,
                    path: None,
                },
                false,
            ),
            (
                "log @1 src --skip 5 --limit 3 --json",
                CheckoutRequest::Log {
                    skip: 5,
                    limit: 3,
                    path: Some("src".into()),
                },
                true,
            ),
            ("branches work", CheckoutRequest::Branches, false),
            ("pr . --json", CheckoutRequest::PullRequest, true),
            (
                "show @1 abc123",
                CheckoutRequest::Show {
                    sha: "abc123".into(),
                },
                false,
            ),
        ] {
            let cli = crate::parse_cli(
                std::iter::once("phux git")
                    .chain([argv])
                    .flat_map(str::split_whitespace),
            )
            .unwrap_or_else(|err| panic!("{argv}: {err:?}"));
            let Some(crate::commands::Command::Git(args)) = cli.command else {
                panic!("{argv}: not a git verb");
            };
            let (_, request, json) = request_of(args.action);
            assert_eq!((request, json), (want, want_json), "{argv}");
        }
    }

    #[test]
    fn staged_and_untracked_conflict() {
        let argv = ["phux", "git", "diff", "@1", "a", "--staged", "--untracked"];
        assert!(crate::parse_cli(argv.map(str::to_owned).to_vec()).is_err());
    }
}
