//! Landing a pull request: merge, auto-merge, ready for review, and
//! checking its branch out here.

use gpui::{Context, Window};

use crate::git::github::{self, MergeMethod, write};

use super::super::Kind;
use super::super::desk::Desk;
use super::PatchFrom;

impl Desk {
    pub(crate) fn set_merge_method(&mut self, method: MergeMethod, cx: &mut Context<Self>) {
        if let Some(opened) = self.reviews.opened.as_mut() {
            opened.method = Some(method);
            opened.confirm_merge = false;
            cx.notify();
        }
    }

    /// The first press arms; the second merges the head that was reviewed,
    /// then fast-forwards the base in the clone here. Branch deletion is
    /// the repository's own setting: GitHub applies it and retargets any
    /// pull request stacked on the branch.
    pub(crate) fn merge_pull(&mut self, cx: &mut Context<Self>) {
        let Some(opened) = self.reviews.opened.as_mut() else {
            return;
        };
        // Never substitute the newest API head for a diff that was not read.
        let Some(reviewed) = opened.patch.data.as_ref().map(|patch| patch.head.clone()) else {
            return;
        };
        if opened.revision_changed() {
            return;
        }
        if !opened.confirm_merge {
            opened.confirm_merge = true;
            cx.notify();
            return;
        }
        opened.confirm_merge = false;
        let (Some(pull), Some(method)) = (opened.pull.data.clone(), opened.merge_method()) else {
            return;
        };
        // A push since makes GitHub refuse rather than merge unseen commits.
        let clone = opened.place.clone.clone();
        let label = opened.key.label();
        self.review_write(
            "Merging",
            move || {
                super::verify_review_base(&pull)?;
                write::merge(&pull.id, method, &reviewed)?;
                let Some((root, remote)) = clone else {
                    return Ok(String::new());
                };
                Ok(
                    match github::patch::fast_forward(&root, &remote, &pull.base_ref) {
                        Ok(message) => message,
                        Err(error) => format!("{} not updated here: {error}", pull.base_ref),
                    },
                )
            },
            move |desk, result, _, cx| {
                match result {
                    Ok(said) => desk.post(
                        Kind::Info,
                        &format!("Merged {label}"),
                        Some(said).filter(|said| !said.is_empty()),
                    ),
                    Err(error) => desk.post(
                        Kind::Error,
                        &format!("Could not merge {label}"),
                        Some(error),
                    ),
                }
                desk.refresh_inbox(cx);
            },
            cx,
        );
    }

    /// Merge once checks and reviews pass, or stop waiting to.
    pub(crate) fn toggle_auto_merge(&mut self, cx: &mut Context<Self>) {
        let Some(opened) = self.reviews.opened.as_ref() else {
            return;
        };
        let Some(pull) = opened.pull.data.as_ref() else {
            return;
        };
        let method = match pull.auto_merge {
            Some(_) => None,
            None => opened.merge_method(),
        };
        if pull.auto_merge.is_none() && method.is_none() {
            return;
        }
        let id = pull.id.clone();
        let what = if method.is_some() {
            "Enabling auto-merge"
        } else {
            "Disabling auto-merge"
        };
        self.review_write(
            what,
            move || write::set_auto_merge(&id, method),
            |desk, result, _, _| {
                if let Err(error) = result {
                    desk.post(Kind::Error, "Could not change auto-merge", Some(error));
                }
            },
            cx,
        );
    }

    pub(crate) fn mark_ready(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self
            .reviews
            .opened
            .as_ref()
            .and_then(|opened| opened.pull.data.as_ref())
            .map(|pull| pull.id.clone())
        else {
            return;
        };
        self.review_write(
            "Marking ready",
            move || write::ready_for_review(&id),
            |desk, result, _, _| {
                if let Err(error) = result {
                    desk.post(Kind::Error, "Could not mark ready", Some(error));
                }
            },
            cx,
        );
    }

    /// Open the branch here: the worktree already on it, else a new linked
    /// worktree of the clone with its own session.
    pub(crate) fn check_out_pull(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(failure) = self.local_only() {
            self.post(Kind::Refusal, "Could not check out", Some(failure));
            cx.notify();
            return;
        }
        let Some(opened) = self.reviews.opened.as_ref() else {
            return;
        };
        let Some(pull) = opened.pull.data.clone() else {
            return;
        };
        if let Some(worktree) = opened.place.worktree.clone() {
            self.close_reviews(window, cx);
            self.open_project_path(worktree, window, cx);
            return;
        }
        let clone = opened
            .place
            .clone
            .clone()
            .or_else(|| match &opened.patch.data {
                Some(patch) => match &patch.from {
                    PatchFrom::Local(root) => Some((root.clone(), "origin".to_owned())),
                    PatchFrom::GitHub => None,
                },
                None => None,
            });
        let Some((root, remote)) = clone else {
            self.post(
                Kind::Refusal,
                &format!("No clone on {}", crate::platform::this_machine()),
                Some(format!(
                    "Clone {} first, or open a terminal in its clone.",
                    pull.repo
                )),
            );
            cx.notify();
            return;
        };
        let branch = super::checkout_branch(&pull);
        let (phux, socket) = (self.phux.clone(), self.socket.clone());
        self.review_write(
            "Checking out",
            move || {
                let range = github::patch::Range {
                    number: pull.number,
                    base_ref: &pull.base_ref,
                    base_oid: &pull.base_oid,
                    head_oid: &pull.head_oid,
                };
                // Bring the head here; the diff may have been GitHub's.
                github::patch::fetch_commits(&root, &remote, &range)
                    .map_err(github::GhError::Failed)?;
                super::super::host::new_worktree(&phux, &socket, &root, &branch, &pull.head_oid)
                    .map_err(github::GhError::Failed)
            },
            |desk, result, _, cx| match result {
                Ok(opened) => {
                    desk.reviews.open = false;
                    desk.open_project_window(opened.session, opened.terminal, cx);
                }
                Err(error) => desk.post(Kind::Error, "Could not check out", Some(error)),
            },
            cx,
        );
    }
}
