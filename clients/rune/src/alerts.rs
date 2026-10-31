//! System notifications for what needs you while you are looking elsewhere,
//! and the Dock's count.
//!
//! An alert is posted once, when its item first appears out of sight, and
//! taken back when it is answered or comes into view. Clicks arrive on a
//! system thread, so they queue here for the next poll.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use phux_protocol::ids::ResourceId;
use rune::Waker;
use rune_mac::notify::{self, Notification};

use crate::activity::Mark;
use crate::inbox::{self, Item};

/// How long after a connection opens items count as old news: the server
/// replays what was already waiting.
const SETTLE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Terminal { id: ResourceId, session: String },
    Inbox,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alert {
    pub id: String,
    pub title: String,
    pub body: String,
    pub target: Target,
}

/// The alert an item raises. Bells stay in the app.
pub fn alert(item: &Item) -> Option<Alert> {
    match item {
        Item::Approval(approval) => Some(Alert {
            id: format!("approval:{}", approval.id),
            title: "Approval needed".to_owned(),
            body: format!(
                "{} wants: {}",
                inbox::approval_requester(approval),
                inbox::approval_title(approval)
            ),
            target: Target::Inbox,
        }),
        Item::Terminal(waiting) => {
            let who = waiting.agent.as_deref().unwrap_or(&waiting.label);
            let (key, title, body) = match (waiting.mark, &waiting.ask) {
                (Mark::Blocked, Some(ask)) => (
                    format!("ask:{}", ask.id),
                    format!("{who} is asking"),
                    ask.text.clone(),
                ),
                (Mark::Blocked, None) => (
                    "blocked".to_owned(),
                    format!("{who} needs you"),
                    waiting.label.clone(),
                ),
                (Mark::Done, _) => (
                    "done".to_owned(),
                    format!("{who} finished"),
                    waiting.label.clone(),
                ),
                (Mark::Failed, _) => (
                    "failed".to_owned(),
                    "Command failed".to_owned(),
                    waiting.label.clone(),
                ),
                _ => return None,
            };
            Some(Alert {
                id: format!("{}:{key}", waiting.terminal),
                title,
                body,
                target: Target::Terminal {
                    id: waiting.terminal.clone(),
                    session: waiting.session.clone(),
                },
            })
        }
    }
}

/// What one sync asks of the system.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Changes {
    pub post: Vec<Alert>,
    pub remove: Vec<String>,
}

/// Which alerts exist and which are in Notification Center.
#[derive(Debug, Default)]
pub struct Tracker {
    known: HashSet<String>,
    delivered: HashSet<String>,
    targets: HashMap<String, Target>,
}

impl Tracker {
    /// Fold the current list in. `visible` says the person can see an alert's
    /// subject now; `quiet` records without posting.
    pub fn sync(
        &mut self,
        alerts: Vec<Alert>,
        visible: impl Fn(&Target) -> bool,
        quiet: bool,
    ) -> Changes {
        let mut changes = Changes::default();
        let present: HashSet<String> = alerts.iter().map(|a| a.id.clone()).collect();
        self.known.retain(|id| present.contains(id));
        self.targets.retain(|id, _| present.contains(id));
        let gone: Vec<String> = self
            .delivered
            .iter()
            .filter(|id| !present.contains(*id))
            .cloned()
            .collect();
        for id in gone {
            self.delivered.remove(&id);
            changes.remove.push(id);
        }
        for alert in alerts {
            let seen = visible(&alert.target);
            let new = self.known.insert(alert.id.clone());
            self.targets.insert(alert.id.clone(), alert.target.clone());
            if seen {
                if self.delivered.remove(&alert.id) {
                    changes.remove.push(alert.id);
                }
            } else if new && !quiet {
                self.delivered.insert(alert.id.clone());
                changes.post.push(alert);
            }
        }
        changes
    }

    pub fn target(&self, id: &str) -> Option<Target> {
        self.targets.get(id).cloned()
    }
}

/// The system side: posting, clicks, and the Dock.
pub struct Alerts {
    tracker: Tracker,
    clicks: Arc<Mutex<Vec<String>>>,
    quiet_until: Option<Instant>,
    badge: usize,
}

impl Default for Alerts {
    fn default() -> Self {
        Self {
            tracker: Tracker::default(),
            clicks: Arc::new(Mutex::new(Vec::new())),
            quiet_until: None,
            badge: 0,
        }
    }
}

impl Alerts {
    /// Hear clicks, waking the app's loop for each.
    pub fn listen(&self, waker: Waker) {
        let clicks = self.clicks.clone();
        notify::on_click(move |click| {
            clicks
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(click.id);
            waker();
        });
    }

    /// A connection opened: what it replays is not news.
    pub fn connected(&mut self) {
        self.quiet_until = Some(Instant::now() + SETTLE);
    }

    pub fn sync(&mut self, items: &[Item], visible: impl Fn(&Target) -> bool, enabled: bool) {
        let alerts = items.iter().filter_map(alert).collect();
        let quiet = !enabled || self.quiet_until.is_some_and(|t| Instant::now() < t);
        let changes = self.tracker.sync(alerts, visible, quiet);
        for alert in changes.post {
            notify::post(&Notification::new(alert.id, alert.title).body(alert.body));
        }
        let gone: Vec<&str> = changes.remove.iter().map(String::as_str).collect();
        notify::remove(&gone);
        if items.len() != self.badge {
            self.badge = items.len();
            crate::dock::badge(self.badge);
        }
    }

    /// Where the clicks since the last call lead.
    pub fn take_clicks(&mut self) -> Vec<Target> {
        let clicks = std::mem::take(&mut *self.clicks.lock().unwrap_or_else(|e| e.into_inner()));
        clicks
            .iter()
            .filter_map(|id| self.tracker.target(id))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal(n: u32) -> Target {
        Target::Terminal {
            id: ResourceId::local(n),
            session: "s".to_owned(),
        }
    }

    fn raised(id: &str, target: Target) -> Alert {
        Alert {
            id: id.to_owned(),
            title: String::new(),
            body: String::new(),
            target,
        }
    }

    #[test]
    fn posts_once_and_takes_it_back_when_gone() {
        let mut t = Tracker::default();
        let a = raised("a", terminal(1));
        let first = t.sync(vec![a.clone()], |_| false, false);
        assert_eq!(first.post, vec![a.clone()]);
        assert!(t.sync(vec![a], |_| false, false).post.is_empty());
        let gone = t.sync(Vec::new(), |_| false, false);
        assert_eq!(gone.remove, vec!["a".to_owned()]);
        assert!(t.target("a").is_none());
    }

    #[test]
    fn nothing_posts_for_what_you_can_see() {
        let mut t = Tracker::default();
        let a = raised("a", terminal(1));
        assert!(t.sync(vec![a.clone()], |_| true, false).post.is_empty());
        // Looking away from something already known does not raise it.
        assert!(t.sync(vec![a], |_| false, false).post.is_empty());
    }

    #[test]
    fn looking_at_a_delivered_alert_takes_it_back() {
        let mut t = Tracker::default();
        let a = raised("a", terminal(1));
        t.sync(vec![a.clone()], |_| false, false);
        let seen = t.sync(vec![a], |_| true, false);
        assert_eq!(seen.remove, vec!["a".to_owned()]);
    }

    #[test]
    fn a_replay_while_quiet_stays_quiet_afterwards() {
        let mut t = Tracker::default();
        let a = raised("a", terminal(1));
        assert!(t.sync(vec![a.clone()], |_| false, true).post.is_empty());
        assert!(t.sync(vec![a], |_| false, false).post.is_empty());
    }

    #[test]
    fn a_new_question_is_a_new_alert() {
        let mut t = Tracker::default();
        t.sync(vec![raised("a", terminal(1))], |_| false, false);
        let next = t.sync(vec![raised("b", terminal(1))], |_| false, false);
        assert_eq!(next.post.len(), 1);
        assert_eq!(next.remove, vec!["a".to_owned()]);
    }

    #[test]
    fn a_click_finds_its_target() {
        let mut t = Tracker::default();
        t.sync(vec![raised("a", Target::Inbox)], |_| false, false);
        assert_eq!(t.target("a"), Some(Target::Inbox));
    }
}
