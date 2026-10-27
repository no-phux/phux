//! Reading GitHub and the local clone, off the UI thread.

use std::path::PathBuf;

use gpui::Context;
use gpui::prelude::*;
use gpui_kit::component::diff::{DiffEvent, DiffInlineUnit, DiffState};

use crate::git::PatchText;
use crate::git::github::{self, Pull, patch};

use super::super::changes::review::{self, Loaded};
use super::super::desk::Desk;
use super::local::{self, Candidate, Pane};
use super::{Agent, Key, Log, Patch, PatchFrom, Place};

impl Desk {
    /// Re-read the inbox; the shown one stays until the answer lands.
    pub(crate) fn refresh_inbox(&mut self, cx: &mut Context<Self>) {
        if self.reviews.inbox.loading || self.reviews.offline {
            return;
        }
        self.reviews.inbox.start();
        let owners = self.reviews.owners.clone();
        cx.spawn(async move |this, cx| {
            let result = smol::unblock(move || {
                let owners = match owners {
                    Some(owners) => owners,
                    None => github::inbox::owners().map_err(|error| error.to_string())?,
                };
                let inbox = github::inbox::fetch(&owners).map_err(|error| error.to_string())?;
                Ok::<_, String>((owners, inbox))
            })
            .await;
            let _ = this.update(cx, |desk, cx| {
                let result = result.map(|(owners, inbox)| {
                    desk.reviews.owners = Some(owners);
                    inbox
                });
                desk.reviews.inbox.finish(result);
                if desk.reviews.cursor.is_none() {
                    desk.reviews.cursor = desk.reviews.ordered().into_iter().next();
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Folders that may hold clones of `repo`, and the panes in them.
    fn candidates(&self, repo: &str) -> Vec<Candidate> {
        let mut candidates = Vec::new();
        if self.remote.is_some() {
            return candidates;
        }
        if let Some(link) = self.link.as_ref() {
            let connection = &link.connection;
            for pane in connection.panes() {
                let Some(cwd) = pane.cwd.as_deref().filter(|_| pane.terminal_id.is_local()) else {
                    continue;
                };
                let agent = connection.agents.contains_key(&pane.terminal_id);
                candidates.push(Candidate {
                    dir: super::super::changes::expand_home(cwd),
                    pane: Some(Pane {
                        terminal: pane.terminal_id.clone(),
                        title: self.pane_title(Some(&pane.terminal_id)),
                        agent,
                    }),
                });
            }
        }
        let recent = self.recent_projects.iter().cloned();
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let usual = home.iter().flat_map(|home| local::conventional(home, repo));
        candidates.extend(recent.chain(usual).map(|dir| Candidate { dir, pane: None }));
        candidates
    }

    /// Re-read `key`: its detail, where it lives here, and its patch when the
    /// head moved. A result for a pull request no longer open is dropped.
    pub(crate) fn read_pull(&mut self, key: &Key, cx: &mut Context<Self>) {
        let Some(opened) = self.reviews.opened.as_mut().filter(|o| &o.key == key) else {
            return;
        };
        if opened.pull.loading || self.reviews.offline {
            return;
        }
        opened.pull.start();
        let generation = opened.generation;
        let candidates = self.candidates(&key.repo);
        let host = phux_client_runtime::organize::local_host_name();
        let key = key.clone();
        cx.spawn(async move |this, cx| {
            let read = key.clone();
            let result = smol::unblock(move || {
                let pull =
                    github::pull::fetch(&read.repo, read.number).map_err(|e| e.to_string())?;
                let place = place(&read.repo, &pull, &candidates, &host);
                Ok::<_, String>((pull, place))
            })
            .await;
            let _ = this.update(cx, |desk, cx| {
                let Some(opened) = desk
                    .reviews
                    .opened
                    .as_mut()
                    .filter(|o| o.generation == generation)
                else {
                    return;
                };
                let (result, place) = match result {
                    Ok((pull, place)) => (Ok(pull), Some(place)),
                    Err(error) => (Err(error), None),
                };
                let unchanged =
                    matches!((&result, &opened.pull.data), (Ok(new), Some(old)) if new == old);
                if let Some(place) = place {
                    opened.place = place;
                }
                opened.pull.finish(result);
                // A reread confirms or replaces marks set here.
                // A mark set here stays until GitHub agrees with it, so a
                // reread racing the write does not flip it back.
                if let Some(pull) = opened.pull.data.as_ref() {
                    opened.viewed.retain(|path, viewed| {
                        let on_github = pull.files.iter().any(|file| {
                            &file.path == path
                                && file.viewed == crate::git::github::ViewedState::Viewed
                        });
                        on_github != *viewed
                    });
                }
                let head = opened.pull.data.as_ref().map(|pull| pull.head_oid.clone());
                let stale = head.is_some()
                    && opened.patch.data.as_ref().map(|patch| &patch.head) != head.as_ref();
                if stale && !opened.patch.loading {
                    desk.read_patch(cx);
                }
                if !unchanged {
                    desk.sync_review_diff(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Read the opened pull request's patch: the local clone, else GitHub.
    fn read_patch(&mut self, cx: &mut Context<Self>) {
        let Some(opened) = self.reviews.opened.as_mut() else {
            return;
        };
        let Some(pull) = opened.pull.data.clone() else {
            return;
        };
        opened.patch.start();
        let generation = opened.generation;
        let clone = opened.place.clone.clone();
        cx.spawn(async move |this, cx| {
            let head = pull.head_oid.clone();
            let result = smol::unblock(move || {
                let (from, text) = patch_text(&pull, clone)?;
                let loaded = Loaded::parse(text, Vec::new());
                Ok::<_, String>(Patch { from, loaded, head })
            })
            .await;
            let _ = this.update(cx, |desk, cx| {
                let Some(opened) = desk
                    .reviews
                    .opened
                    .as_mut()
                    .filter(|o| o.generation == generation)
                else {
                    return;
                };
                let before = opened.patch.data.as_ref().map(|patch| patch.head.clone());
                opened.patch.finish(result);
                // Merge is armed for what was on screen; a new head disarms it.
                if opened.patch.data.as_ref().map(|patch| &patch.head) != before.as_ref() {
                    opened.confirm_merge = false;
                }
                desk.show_review_diff(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Put the patch's files in the diff, folding viewed ones, and return to
    /// the file last revealed.
    pub(super) fn show_review_diff(&mut self, cx: &mut Context<Self>) {
        let Some(opened) = self.reviews.opened.as_mut() else {
            return;
        };
        let Some(patch) = &opened.patch.data else {
            return;
        };
        let files = patch.loaded.files.clone();
        let state = match &opened.diff {
            Some((state, _)) => {
                state.update(cx, |state, cx| state.set_files(files, cx));
                state.clone()
            }
            None => {
                let state = cx.new(|cx| {
                    DiffState::new(files, cx)
                        .with_context_lines(Some(review::SHOWN_CONTEXT))
                        .with_inline_unit(Some(DiffInlineUnit::Word))
                });
                let subscription = cx.subscribe(&state, |_, _, event: &DiffEvent, cx| {
                    if matches!(
                        event,
                        DiffEvent::SelectionChanged(_) | DiffEvent::SelectionEnded(_)
                    ) {
                        cx.notify();
                    }
                });
                opened.diff = Some((state.clone(), subscription));
                state
            }
        };
        let focus = opened.focus.clone();
        state.update(cx, |state, cx| {
            if let Some(path) = focus {
                state.scroll_to_file(&path, cx);
            }
        });
        self.sync_review_diff(cx);
    }

    /// Fold viewed files and open the rest, as the marks now say.
    pub(crate) fn sync_review_diff(&mut self, cx: &mut Context<Self>) {
        let Some(opened) = self.reviews.opened.as_ref() else {
            return;
        };
        let (Some((state, _)), Some(patch)) = (&opened.diff, &opened.patch.data) else {
            return;
        };
        let marks: Vec<(String, bool)> = patch
            .loaded
            .files
            .iter()
            .map(|file| (file.path().to_string(), opened.is_viewed(file.path())))
            .collect();
        state.update(cx, |state, cx| {
            for (path, viewed) in marks {
                if state.is_file_collapsed(&path) != viewed {
                    state.set_file_collapsed(&path, viewed, cx);
                }
            }
        });
    }

    /// Read job `job`'s log, cut to what failed.
    pub(crate) fn read_job_log(&mut self, job: u64, cx: &mut Context<Self>) {
        let Some(opened) = self.reviews.opened.as_mut() else {
            return;
        };
        if opened.log.as_ref().is_some_and(|log| log.job == job) {
            opened.log = None;
            cx.notify();
            return;
        }
        let mut log = Log {
            job,
            text: super::Fetched::default(),
        };
        log.text.start();
        opened.log = Some(log);
        let repo = opened.key.repo.clone();
        let generation = opened.generation;
        cx.spawn(async move |this, cx| {
            let result = smol::unblock(move || {
                github::logs::job_excerpt(&repo, job).map_err(|error| error.to_string())
            })
            .await;
            let _ = this.update(cx, |desk, cx| {
                let Some(log) = desk
                    .reviews
                    .opened
                    .as_mut()
                    .filter(|o| o.generation == generation)
                    .and_then(|o| o.log.as_mut())
                    .filter(|log| log.job == job)
                else {
                    return;
                };
                log.text.finish(result);
                cx.notify();
            });
        })
        .detach();
    }
}

/// Where `pull` lives on this Mac, and the agent on its branch.
fn place(repo: &str, pull: &Pull, candidates: &[Candidate], host: &str) -> Place {
    let found = local::find(repo, candidates, host);
    let clone = local::clone_of(&found).and_then(|each| {
        let remote = patch::remote_for(&each.root, repo)?;
        Some((each.root.clone(), remote))
    });
    let branch = local::on_branch(&found, &pull.head_ref).filter(|_| !pull.is_fork());
    // A worktree no pane is in yet still beats making another.
    let worktree = branch.map(|each| each.root.clone()).or_else(|| {
        let (root, _) = clone.as_ref()?;
        patch::worktree_on(root, &super::checkout_branch(pull))
    });
    Place {
        clone,
        worktree,
        agent: branch
            .and_then(|each| each.pane.clone())
            .filter(|pane| pane.agent)
            .map(|pane| Agent {
                terminal: pane.terminal,
                title: pane.title,
            }),
    }
}

/// The patch from the clone with every line of context, else GitHub's.
fn patch_text(
    pull: &Pull,
    clone: Option<(PathBuf, String)>,
) -> Result<(PatchFrom, PatchText), String> {
    if let Some((root, remote)) = clone {
        let range = patch::Range {
            number: pull.number,
            base_ref: &pull.base_ref,
            base_oid: &pull.base_oid,
            head_oid: &pull.head_oid,
        };
        match patch::local(&root, &remote, &range, review::FETCH_CONTEXT) {
            Ok(text) => return Ok((PatchFrom::Local(root), text)),
            Err(error) => log::info!(
                "reviews: local patch for {}#{} failed: {error}",
                pull.repo,
                pull.number
            ),
        }
    }
    let text = patch::remote(&pull.repo, pull.number).map_err(|error| error.to_string())?;
    Ok((PatchFrom::GitHub, text))
}
