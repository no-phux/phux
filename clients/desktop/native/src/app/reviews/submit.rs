//! The review: to GitHub as one submission, and to the agent on the branch.

use std::time::Duration;

use gpui::Context;
use phux_protocol::ids::ResourceId;

use crate::git::github::{NewThread, ReviewEvent, write};

use super::super::Kind;
use super::super::desk::Desk;
use super::Opened;
use super::lines::new_thread;
use super::prompt;

/// How long a paste waits for a terminal shown in another session to attach.
const PASTE_WAIT: Duration = Duration::from_secs(10);
const PASTE_STEP: Duration = Duration::from_millis(100);

impl Opened {
    fn submission_refusal(&self, cx: &gpui::App) -> Option<(&'static str, &'static str)> {
        if self.draft.is_some() {
            return Some((
                "Finish your comment first",
                "Keep or cancel the open comment before submitting your review.",
            ));
        }
        if self.base_changed() {
            return Some((
                "The review target changed",
                "Remove pending line comments and refresh to review the new base before submitting.",
            ));
        }
        if self.event != ReviewEvent::Comment && self.patch.data.is_none() {
            return Some((
                "The diff is not ready",
                "Wait for the diff to load before submitting a verdict.",
            ));
        }
        if self.event == ReviewEvent::Comment
            && self.summary_text(cx).is_empty()
            && self.pending.is_empty()
        {
            return Some((
                "Nothing to submit",
                "Comment on some lines or write a summary first.",
            ));
        }
        None
    }
}

impl Desk {
    /// What the agent gets for the opened review.
    fn review_prompt(&self, cx: &gpui::App) -> Option<String> {
        let opened = self.reviews.opened.as_ref()?;
        let pull = opened.pull.data.as_ref()?;
        let open_threads: Vec<_> = pull
            .threads
            .iter()
            .filter(|thread| !thread.resolved)
            .collect();
        let summary = opened.summary_text(cx);
        let verdict = match opened.event {
            ReviewEvent::Comment => None,
            ReviewEvent::Approve => Some("approved"),
            ReviewEvent::RequestChanges => Some("changes requested"),
        };
        if opened.pending.is_empty() && summary.is_empty() && open_threads.is_empty() {
            return None;
        }
        Some(prompt::prompt(&prompt::Review {
            repo: &pull.repo,
            number: pull.number,
            branch: &pull.head_ref,
            url: &pull.url,
            verdict,
            summary: &summary,
            notes: &opened.pending,
            open_threads: &open_threads,
        }))
    }

    /// Submit the review: its verdict, summary and every kept comment land on
    /// GitHub as one review, at the head the comments were written on, then
    /// go to the branch's agent when asked.
    pub(crate) fn submit_review(&mut self, cx: &mut Context<Self>) {
        let Some(opened) = self.reviews.opened.as_ref() else {
            return;
        };
        if let Some((title, detail)) = opened.submission_refusal(cx) {
            self.post(Kind::Refusal, title, Some(detail.into()));
            cx.notify();
            return;
        }
        let Some(pull) = opened.pull.data.as_ref() else {
            return;
        };
        let pull_id = pull.id.clone();
        let reviewed_pull = pull.clone();
        let head = opened
            .patch
            .data
            .as_ref()
            .map_or_else(|| pull.head_oid.clone(), |patch| patch.head.clone());
        let sections = opened
            .patch
            .data
            .as_ref()
            .map(|patch| &patch.loaded.sections);
        let threads: Vec<NewThread> = opened
            .pending
            .iter()
            .map(|pending| {
                let path = pending.range.path().as_ref();
                new_thread(
                    pending,
                    sections.and_then(|s| s.get(path)).map(String::as_str),
                )
            })
            .collect();
        let event = opened.event;
        let body = opened.summary_text(cx);
        let sent_body = body.clone();
        let agent = opened.place.agent.clone().filter(|_| opened.to_agent);
        let prompt = agent.as_ref().and_then(|_| self.review_prompt(cx));
        let sent = opened
            .pending
            .iter()
            .map(|pending| pending.id)
            .collect::<Vec<_>>();
        self.review_write(
            "Submitting",
            move || {
                super::verify_review_base(&reviewed_pull)?;
                write::submit_review(&pull_id, &head, event, &body, &threads)
            },
            move |desk, result, current, cx| {
                if let Err(error) = result {
                    desk.post(Kind::Error, "Could not submit the review", Some(error));
                    return;
                }
                if current && let Some(opened) = desk.reviews.opened.as_mut() {
                    opened.finish_submission(&sent, event, &sent_body, cx);
                }
                match (agent, prompt) {
                    (Some(agent), Some(prompt)) => desk.paste_to_agent(agent.terminal, prompt, cx),
                    _ => desk.post(Kind::Info, "Review submitted", None),
                }
            },
            cx,
        );
    }

    /// Paste the review into the branch's agent without posting anything.
    pub(crate) fn send_review_to_agent(&mut self, cx: &mut Context<Self>) {
        let Some(agent) = self
            .reviews
            .opened
            .as_ref()
            .and_then(|o| o.place.agent.clone())
        else {
            return;
        };
        let Some(prompt) = self.review_prompt(cx) else {
            self.post(
                Kind::Refusal,
                "Nothing to send",
                Some("Comment on some lines first.".into()),
            );
            cx.notify();
            return;
        };
        // Agent handoff is not a GitHub submission. Keep the review here,
        // including when the pane disappears or cannot accept the paste.
        self.paste_to_agent(agent.terminal, prompt, cx);
    }

    /// Show the agent's pane and paste `prompt` into it, unsubmitted: the
    /// reviewer reads it and presses Enter there. A pane in another session
    /// is navigated to first and pasted into once it attaches.
    fn paste_to_agent(&mut self, terminal: ResourceId, prompt: String, cx: &mut Context<Self>) {
        let title = self.pane_title(Some(&terminal));
        self.reviews.open = false;
        if self.placement_of(&terminal).is_none()
            && let Some(destination) = self.session_of_terminal(&terminal)
        {
            self.open_work_terminal(destination, terminal.clone(), cx);
        } else {
            self.reveal_terminal(&terminal);
        }
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let mut waited = Duration::ZERO;
            loop {
                let Ok(pasted) = this.update(cx, |desk, _| desk.queue_paste(&terminal, &prompt))
                else {
                    return;
                };
                if pasted == Some(true) {
                    let _ = this.update(cx, |desk, cx| {
                        desk.post(
                            Kind::Info,
                            "Review sent",
                            Some(format!(
                                "Pasted into {title}. Press Enter there to send it."
                            )),
                        );
                        cx.notify();
                    });
                    return;
                }
                if waited >= PASTE_WAIT {
                    let _ = this.update(cx, |desk, cx| {
                        desk.post(
                            Kind::Error,
                            "Could not paste the review",
                            Some(format!("{title} did not attach.")),
                        );
                        cx.notify();
                    });
                    return;
                }
                executor.timer(PASTE_STEP).await;
                waited += PASTE_STEP;
            }
        })
        .detach();
        cx.notify();
    }

    /// Where `terminal` lives on this window's server.
    fn session_of_terminal(
        &self,
        terminal: &ResourceId,
    ) -> Option<super::super::fleet::Destination> {
        let connection = &self.link.as_ref()?.connection;
        let topology = connection.topology.as_ref()?;
        let pane = connection
            .panes()
            .iter()
            .find(|pane| &pane.terminal_id == terminal)?;
        let session = topology
            .sessions
            .iter()
            .find(|session| session.id == pane.session_id)?;
        Some(super::super::fleet::Destination {
            host: self.remote.clone(),
            authority: topology.authority,
            key: session.key,
            name: session.name.clone(),
        })
    }
}
