//! Typed checkout methods (`Command::Checkout`).
//!
//! The checkout is the one the named Terminal's PTY child is in, resolved
//! from its live working directory on this server; a client never names a
//! host path. The command is `INPUT` on that Terminal: an `INPUT` holder can
//! already run `git diff` or `git add` in it. A satellite Terminal is routed
//! to its satellite before this runs (`commands.rs`), and the hub refuses a
//! satellite that does not advertise `CHECKOUT`.
//!
//! Git and `gh` run on blocking workers under a small global semaphore. Each
//! child has its own deadline and output cap (`phux_git::checkout`), and
//! the answer is bounded again before it is encoded.

use phux_terminal::actor::TerminalResource as _;
use std::path::PathBuf;
use std::time::Duration;

use phux_git::Area as GitArea;
use phux_git::checkout::{self as git, Checkout, CheckoutError};
use phux_protocol::checkout::{
    CheckoutAnswer, CheckoutArea, CheckoutBranch, CheckoutBranches, CheckoutCheck,
    CheckoutCheckState, CheckoutCommit, CheckoutDiff, CheckoutEntry, CheckoutEntryKind,
    CheckoutFile, CheckoutLog, CheckoutPrState, CheckoutPullRequest, CheckoutRefusal,
    CheckoutRefusalCode, CheckoutRequest, CheckoutReviewDecision, CheckoutStatus, CheckoutTree,
    CheckoutTreeEntry, MAX_CHECKOUT_ANSWER_BYTES, encoded_answer_len,
};
use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::wire::frame::{CommandResult, CommandValue};
use tokio::sync::Semaphore;

use crate::state::SharedState;

/// Checkout operations running at once across every connection.
static CHECKOUT_WORKERS: Semaphore = Semaphore::const_new(8);
/// How long the Terminal actor has to report its working directory.
const CWD_DEADLINE: Duration = Duration::from_secs(2);
/// An outer bound past the children's own deadlines: a stage is one write
/// (30 s plus its `SIGTERM` grace) and then a full status read.
const WORK_DEADLINE: Duration = Duration::from_secs(90);

/// Answer one checkout request for a local Terminal.
pub(in crate::runtime) async fn handle(
    state: &SharedState,
    terminal_id: &WireResourceId,
    request: CheckoutRequest,
) -> CommandResult {
    let handle = state.with(|s| {
        s.resource_from_wire(terminal_id)
            .and_then(|core| s.resource_handle(core).cloned())
    });
    let Some(handle) = handle else {
        return crate::runtime::commands::terminal_not_found(terminal_id);
    };
    if let Err(error) = handle.terminal() {
        return crate::runtime::commands::wrong_resource_kind(error);
    }
    let cwd = tokio::time::timeout(CWD_DEADLINE, crate::runtime::attach::query_pane_cwd(handle))
        .await
        .ok()
        .flatten();
    let answer = match cwd {
        None => refused(
            CheckoutRefusalCode::CwdUnknown,
            "this terminal's working directory is not known",
        ),
        Some(cwd) => run_bounded(PathBuf::from(cwd), request).await,
    };
    CommandResult::OkWith(CommandValue::Checkout(Box::new(answer)))
}

async fn run_bounded(cwd: PathBuf, request: CheckoutRequest) -> CheckoutAnswer {
    let Ok(permit) = CHECKOUT_WORKERS.try_acquire() else {
        return refused(
            CheckoutRefusalCode::Busy,
            "too many checkout operations are running; retry",
        );
    };
    let work = tokio::task::spawn_blocking(move || {
        // Held until the blocking work returns, not until the wait ends.
        let _permit = permit;
        // Fitting serializes the answer, which can be megabytes: keep it on
        // the blocking worker too.
        fit(answer(&cwd, request))
    });
    match tokio::time::timeout(WORK_DEADLINE, work).await {
        Ok(Ok(answer)) => answer,
        Ok(Err(error)) => refused(
            CheckoutRefusalCode::Unknown,
            &format!("checkout worker failed: {error}"),
        ),
        Err(_) => refused(
            CheckoutRefusalCode::Timeout,
            "the checkout operation did not finish in time",
        ),
    }
}

/// Index writes run one at a time: two clients staging at once would
/// otherwise race for `index.lock` and one would fail spuriously. Reads take
/// no lock (`GIT_OPTIONAL_LOCKS=0`) and run beside them.
static INDEX_WRITES: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Run `request` against the checkout containing `cwd`. Blocking.
fn answer(cwd: &std::path::Path, request: CheckoutRequest) -> CheckoutAnswer {
    let _write = request.mutates().then(|| {
        INDEX_WRITES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    });
    let result = Checkout::open(cwd).and_then(|checkout| dispatch(&checkout, request));
    result.unwrap_or_else(|error| refusal(&error))
}

fn dispatch(
    checkout: &Checkout,
    request: CheckoutRequest,
) -> Result<CheckoutAnswer, CheckoutError> {
    Ok(match request {
        CheckoutRequest::Status => CheckoutAnswer::Status(status(checkout)?),
        CheckoutRequest::Diff { path, area } => {
            let git_area = git_area(area).ok_or_else(|| {
                CheckoutError::InvalidRequest(
                    "name the conflicted, staged, unstaged, or untracked area".to_owned(),
                )
            })?;
            let diff = checkout.diff(&path, git_area)?;
            CheckoutAnswer::Diff(CheckoutDiff {
                path,
                area,
                text: diff.text,
                truncated: diff.truncated,
                binary: diff.binary,
            })
        }
        CheckoutRequest::Log { skip, limit, path } => {
            CheckoutAnswer::Log(log(checkout.log(skip, limit, path.as_deref())?))
        }
        CheckoutRequest::Branches => CheckoutAnswer::Branches(branches(checkout.branches()?)),
        CheckoutRequest::Tree { dir } => {
            let listing = checkout.tree(&dir)?;
            CheckoutAnswer::Tree(tree(dir, listing))
        }
        CheckoutRequest::Read { path, max_bytes } => {
            let file = checkout.read(&path, max_bytes)?;
            CheckoutAnswer::File(CheckoutFile {
                path,
                size: file.size,
                binary: file.binary,
                truncated: file.truncated,
                text: file.text,
            })
        }
        CheckoutRequest::PullRequest => {
            CheckoutAnswer::PullRequest(pull_request(checkout.pull_request()?))
        }
        CheckoutRequest::Stage { paths } => {
            checkout.stage(&paths)?;
            CheckoutAnswer::Status(status(checkout)?)
        }
        CheckoutRequest::Unstage { paths } => {
            checkout.unstage(&paths)?;
            CheckoutAnswer::Status(status(checkout)?)
        }
    })
}

fn status(checkout: &Checkout) -> Result<CheckoutStatus, CheckoutError> {
    let snapshot = checkout.status()?;
    Ok(CheckoutStatus {
        root: checkout.root().to_string_lossy().into_owned(),
        branch: snapshot.branch,
        head: snapshot.head,
        upstream: snapshot.upstream,
        ahead: snapshot.ahead,
        behind: snapshot.behind,
        entries: snapshot
            .changes
            .into_iter()
            .map(|change| CheckoutEntry {
                path: change.path,
                from: change.from,
                area: wire_area(change.area),
                code: change.code.to_string(),
                added: change.added,
                deleted: change.deleted,
                binary: change.binary,
            })
            .collect(),
        truncated: snapshot.truncated,
    })
}

const fn git_area(area: CheckoutArea) -> Option<GitArea> {
    Some(match area {
        CheckoutArea::Conflicted => GitArea::Conflicted,
        CheckoutArea::Staged => GitArea::Staged,
        CheckoutArea::Unstaged => GitArea::Unstaged,
        CheckoutArea::Untracked => GitArea::Untracked,
        CheckoutArea::Unknown => return None,
    })
}

const fn wire_area(area: GitArea) -> CheckoutArea {
    match area {
        GitArea::Conflicted => CheckoutArea::Conflicted,
        GitArea::Staged => CheckoutArea::Staged,
        GitArea::Unstaged => CheckoutArea::Unstaged,
        GitArea::Untracked => CheckoutArea::Untracked,
    }
}

fn log(page: git::LogPage) -> CheckoutLog {
    CheckoutLog {
        commits: page
            .commits
            .into_iter()
            .map(|commit| CheckoutCommit {
                sha: commit.sha,
                short_sha: commit.short_sha,
                subject: commit.subject,
                author_name: commit.author_name,
                author_email: commit.author_email,
                time: commit.time,
                refs: commit.refs,
            })
            .collect(),
        has_more: page.has_more,
    }
}

fn branches(list: git::BranchList) -> CheckoutBranches {
    CheckoutBranches {
        current: list.current,
        branches: list
            .branches
            .into_iter()
            .map(|branch| CheckoutBranch {
                name: branch.name,
                remote: branch.remote,
                current: branch.current,
                upstream: branch.upstream,
                ahead: branch.ahead,
                behind: branch.behind,
                sha: branch.sha,
                time: branch.time,
                subject: branch.subject,
            })
            .collect(),
        truncated: list.truncated,
    }
}

fn tree(dir: String, listing: git::Listing) -> CheckoutTree {
    CheckoutTree {
        dir,
        entries: listing
            .entries
            .into_iter()
            .map(|entry| CheckoutTreeEntry {
                name: entry.name,
                kind: match entry.kind {
                    git::EntryKind::File => CheckoutEntryKind::File,
                    git::EntryKind::Directory => CheckoutEntryKind::Directory,
                    git::EntryKind::Symlink => CheckoutEntryKind::Symlink,
                },
                size: entry.size,
                ignored: entry.ignored,
            })
            .collect(),
        truncated: listing.truncated,
    }
}

fn pull_request(pr: git::PullRequest) -> CheckoutPullRequest {
    CheckoutPullRequest {
        number: pr.number,
        title: pr.title,
        state: match pr.state {
            git::PrState::Open => CheckoutPrState::Open,
            git::PrState::Closed => CheckoutPrState::Closed,
            git::PrState::Merged => CheckoutPrState::Merged,
            git::PrState::Unknown => CheckoutPrState::Unknown,
        },
        url: pr.url,
        head_ref: pr.head_ref,
        is_draft: pr.is_draft,
        review_decision: pr.review_decision.map(|decision| match decision {
            git::ReviewDecision::Approved => CheckoutReviewDecision::Approved,
            git::ReviewDecision::ChangesRequested => CheckoutReviewDecision::ChangesRequested,
            git::ReviewDecision::ReviewRequired => CheckoutReviewDecision::ReviewRequired,
            git::ReviewDecision::Unknown => CheckoutReviewDecision::Unknown,
        }),
        checks: pr
            .checks
            .into_iter()
            .map(|check| CheckoutCheck {
                name: check.name,
                workflow: check.workflow,
                state: check_state(check.state),
                url: check.url,
            })
            .collect(),
    }
}

const fn check_state(state: git::CheckState) -> CheckoutCheckState {
    match state {
        git::CheckState::Pending => CheckoutCheckState::Pending,
        git::CheckState::Success => CheckoutCheckState::Success,
        git::CheckState::Failure => CheckoutCheckState::Failure,
        git::CheckState::Neutral => CheckoutCheckState::Neutral,
        git::CheckState::Skipped => CheckoutCheckState::Skipped,
        git::CheckState::Cancelled => CheckoutCheckState::Cancelled,
        git::CheckState::Unknown => CheckoutCheckState::Unknown,
    }
}

fn refusal(error: &CheckoutError) -> CheckoutAnswer {
    let code = match error {
        CheckoutError::NotFound(_) => CheckoutRefusalCode::NotFound,
        CheckoutError::NotARepository => CheckoutRefusalCode::NotARepository,
        CheckoutError::InvalidPath(_) => CheckoutRefusalCode::InvalidPath,
        CheckoutError::InvalidRequest(_) => CheckoutRefusalCode::InvalidRequest,
        CheckoutError::Timeout => CheckoutRefusalCode::Timeout,
        CheckoutError::Git(_) => CheckoutRefusalCode::GitFailed,
        CheckoutError::Io(_) => CheckoutRefusalCode::ReadFailed,
        CheckoutError::GhUnavailable => CheckoutRefusalCode::GhUnavailable,
        CheckoutError::GhUnauthenticated(_) => CheckoutRefusalCode::GhUnauthenticated,
        CheckoutError::NoPullRequest => CheckoutRefusalCode::NoPullRequest,
        CheckoutError::Gh(_) => CheckoutRefusalCode::GhFailed,
    };
    refused(code, &error.to_string())
}

fn refused(code: CheckoutRefusalCode, message: &str) -> CheckoutAnswer {
    CheckoutAnswer::Refused(CheckoutRefusal {
        code,
        message: message.to_owned(),
    })
}

/// Shrink `answer` until it encodes within [`MAX_CHECKOUT_ANSWER_BYTES`]:
/// JSON escaping can grow a 1 MiB text several times. Text halves at a line
/// boundary and lists halve, each flagged truncated.
fn fit(mut answer: CheckoutAnswer) -> CheckoutAnswer {
    for _ in 0..8 {
        if encoded_answer_len(&answer) <= MAX_CHECKOUT_ANSWER_BYTES {
            return answer;
        }
        shrink(&mut answer);
    }
    if encoded_answer_len(&answer) <= MAX_CHECKOUT_ANSWER_BYTES {
        answer
    } else {
        refused(
            CheckoutRefusalCode::InvalidRequest,
            "the answer is too large to send",
        )
    }
}

fn shrink(answer: &mut CheckoutAnswer) {
    match answer {
        CheckoutAnswer::Diff(diff) => {
            halve_text(&mut diff.text);
            diff.truncated = true;
        }
        CheckoutAnswer::File(file) => {
            halve_text(&mut file.text);
            file.truncated = true;
        }
        CheckoutAnswer::Status(status) => {
            halve(&mut status.entries);
            status.truncated = true;
        }
        CheckoutAnswer::Branches(list) => {
            halve(&mut list.branches);
            list.truncated = true;
        }
        CheckoutAnswer::Tree(tree) => {
            halve(&mut tree.entries);
            tree.truncated = true;
        }
        CheckoutAnswer::Log(log) => {
            halve(&mut log.commits);
            log.has_more = true;
        }
        CheckoutAnswer::PullRequest(pr) => halve(&mut pr.checks),
        CheckoutAnswer::Refused(refusal) => halve_text(&mut refusal.message),
    }
}

fn halve<T>(items: &mut Vec<T>) {
    items.truncate(items.len() / 2);
}

/// Keep about half of `text`, ending at a line boundary when there is one.
fn halve_text(text: &mut String) {
    let mut cut = text.len() / 2;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    let cut = text[..cut].rfind('\n').map_or(cut, |newline| newline + 1);
    text.truncate(cut);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_oversized_answer_is_cut_to_fit() {
        // Control characters escape to six bytes each in JSON.
        let line = "\u{1}".repeat(1023) + "\n";
        let text = line.repeat(1024);
        let answer = fit(CheckoutAnswer::Diff(CheckoutDiff {
            path: "a".to_owned(),
            area: CheckoutArea::Unstaged,
            text,
            truncated: false,
            binary: false,
        }));
        assert!(encoded_answer_len(&answer) <= MAX_CHECKOUT_ANSWER_BYTES);
        let CheckoutAnswer::Diff(diff) = answer else {
            panic!("still a diff");
        };
        assert!(diff.truncated);
        assert!(diff.text.ends_with('\n'));
    }

    #[test]
    fn git_errors_become_typed_refusals() {
        let refused = |error| match refusal(&error) {
            CheckoutAnswer::Refused(refusal) => refusal.code,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            refused(CheckoutError::NotARepository),
            CheckoutRefusalCode::NotARepository
        );
        assert_eq!(
            refused(CheckoutError::NoPullRequest),
            CheckoutRefusalCode::NoPullRequest
        );
        assert_eq!(
            refused(CheckoutError::Io("denied".to_owned())),
            CheckoutRefusalCode::ReadFailed
        );
        assert_eq!(git_area(CheckoutArea::Unknown), None);
    }
}
