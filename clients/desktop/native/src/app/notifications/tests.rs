//! Notification attention, deduplication, expiry and retention contracts.

use super::*;

fn attention(state: AgentState, level: AttentionLevel) -> AgentAttention {
    AgentAttention {
        state,
        attention: level,
    }
}

#[test]
fn initially_actionable_agents_alert_but_quiet_or_focused_agents_do_not() {
    assert!(attention(AgentState::Blocked, AttentionLevel::Normal).should_alert(None, false));
    assert!(attention(AgentState::Done, AttentionLevel::Normal).should_alert(None, false));
    assert!(attention(AgentState::Working, AttentionLevel::High).should_alert(None, false));
    assert!(!attention(AgentState::Working, AttentionLevel::Normal).should_alert(None, false));
    assert!(!attention(AgentState::Blocked, AttentionLevel::Normal).should_alert(None, true));
}

#[test]
fn attention_escalation_alerts_without_a_state_change_and_deduplicates() {
    let previous = attention(AgentState::Working, AttentionLevel::Normal);
    let escalated = attention(AgentState::Working, AttentionLevel::High);
    assert!(escalated.should_alert(Some(&previous), false));
    assert!(!escalated.should_alert(Some(&escalated), false));
    assert!(!previous.should_alert(Some(&escalated), false));
    assert!(!previous.actionable());
}

#[test]
fn attention_downgrade_does_not_repeat_an_actionable_state() {
    let previous = attention(AgentState::Blocked, AttentionLevel::High);
    assert!(
        !attention(AgentState::Blocked, AttentionLevel::Normal)
            .should_alert(Some(&previous), false)
    );
    assert!(
        attention(AgentState::Done, AttentionLevel::Normal).should_alert(Some(&previous), false)
    );
}

#[test]
fn only_interrupting_entries_peek_and_count() {
    let mut feed = Feed::default();
    feed.post(Kind::Info, "zsh ended", None);
    assert!(feed.peek().is_none());
    assert_eq!(feed.unread().count(), 0);
    let error = feed.post(Kind::Error, "Detached", Some("gone".into()));
    assert_eq!(feed.peek().map(|entry| entry.id), Some(error));
    assert_eq!(feed.unread().count(), 1);
    assert_eq!(feed.items().count(), 2);
    assert_eq!(
        feed.take_outbound()
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        vec![error]
    );
    assert!(feed.take_outbound().is_empty());
}

#[test]
fn a_newer_agent_state_replaces_the_older_entry() {
    let mut feed = Feed::default();
    let terminal = ResourceId::local(7);
    feed.post_agent(
        "a needs you".into(),
        "b".into(),
        terminal.clone(),
        AgentState::Blocked,
    );
    let done = feed.post_agent("a finished".into(), "b".into(), terminal, AgentState::Done);
    assert_eq!(
        feed.items().map(|entry| entry.id).collect::<Vec<_>>(),
        vec![done]
    );
    assert_eq!(feed.take_outbound().len(), 1);
}

#[test]
fn a_long_command_finishing_in_the_background_alerts() {
    let mut commands = Commands::default();
    let terminal = ResourceId::local(7);
    let start = Instant::now();
    commands.started(terminal.clone(), start);
    assert_eq!(
        commands.finished(&terminal, start + COMMAND_ATTENTION, false),
        Some(COMMAND_ATTENTION)
    );
    assert_eq!(
        commands.finished(&terminal, start + COMMAND_ATTENTION, false),
        None,
        "one start alerts once"
    );
}

#[test]
fn a_focused_short_or_unseen_command_does_not_alert() {
    let mut commands = Commands::default();
    let terminal = ResourceId::local(7);
    let start = Instant::now();
    commands.started(terminal.clone(), start);
    assert_eq!(
        commands.finished(&terminal, start + COMMAND_ATTENTION, true),
        None
    );
    commands.started(terminal.clone(), start);
    assert_eq!(
        commands.finished(&terminal, start + Duration::from_secs(1), false),
        None
    );
    assert_eq!(
        commands.finished(&terminal, start + COMMAND_ATTENTION, false),
        None,
        "a finish without a start this window saw"
    );
    commands.started(terminal.clone(), start);
    commands.forget(&terminal);
    assert_eq!(
        commands.finished(&terminal, start + COMMAND_ATTENTION, false),
        None
    );
}

#[test]
fn a_command_entry_interrupts_and_replaces_its_terminals_older_one() {
    let mut feed = Feed::default();
    let terminal = ResourceId::local(7);
    feed.post_command("make finished".into(), "a".into(), terminal.clone());
    let newer = feed.post_command("make finished".into(), "b".into(), terminal);
    assert_eq!(
        feed.items().map(|entry| entry.id).collect::<Vec<_>>(),
        vec![newer]
    );
    assert_eq!(feed.peek().map(|entry| entry.id), Some(newer));
    assert_eq!(feed.unread().count(), 1);
    assert_eq!(feed.take_outbound().len(), 1);
}

#[test]
fn an_approval_is_one_interrupting_inbox_entry() {
    let mut feed = Feed::default();
    let id = ApprovalId::new([7; 16]).unwrap();
    let first = feed.post_approval("Approve kill?".into(), "agent".into(), id);
    assert_eq!(
        feed.post_approval("again".into(), "agent".into(), id),
        first
    );
    assert_eq!(feed.items().count(), 1);
    assert_eq!(feed.peek().map(|entry| entry.id), Some(first));
    assert_eq!(feed.unread().count(), 1);
    assert!(Kind::Approval.inbox() && Kind::Agent.inbox() && !Kind::Error.inbox());
    assert_eq!(feed.get(first).and_then(|entry| entry.approval), Some(id));
}

#[test]
fn an_approval_tag_names_the_approval_not_the_window() {
    let mut feed = Feed::default();
    let id = ApprovalId::new([7; 16]).unwrap();
    let entry = feed.post_approval("Approve kill?".into(), "agent".into(), id);
    let error = feed.post(Kind::Error, "Detached", None);
    let approval = feed.get(entry).cloned().unwrap();
    let tag = SystemTag::of(4, &approval);
    assert_eq!(tag, SystemTag::of(9, &approval));
    assert_eq!(SystemTag::parse(&tag.to_string()), Some(tag));
    assert_eq!(feed.approval_entry(id), Some(entry));
    let other = SystemTag::of(4, feed.get(error).unwrap());
    assert_eq!(other.to_string(), format!("4/{error}"));
    assert_eq!(SystemTag::parse(&other.to_string()), Some(other));
    assert_eq!(SystemTag::parse("approval/zz"), None);
    assert_eq!(SystemTag::parse("4"), None);
}

#[test]
fn reading_clears_the_peek_and_the_count() {
    let mut feed = Feed::default();
    let first = feed.post(Kind::Error, "one", None);
    feed.post(Kind::Error, "two", None);
    feed.read(first);
    assert_eq!(feed.unread().count(), 1);
    assert!(feed.peek().is_some());
    feed.read_all();
    assert_eq!(feed.unread().count(), 0);
    assert!(feed.peek().is_none());
    assert_eq!(feed.items().count(), 2);
}

#[test]
fn removed_entries_leave_no_peek_or_outbound() {
    let mut feed = Feed::default();
    feed.post(Kind::Error, "one", None);
    feed.retain(|_| false);
    assert!(feed.peek().is_none());
    assert!(feed.take_outbound().is_empty());
}

#[test]
fn the_peek_expires_and_the_entry_stays() {
    let mut feed = Feed::default();
    feed.post(Kind::Error, "one", None);
    assert!(!feed.expire_peek(Instant::now()));
    assert!(feed.expire_peek(Instant::now() + PEEK));
    assert!(feed.peek().is_none());
    assert_eq!(feed.unread().count(), 1);
}

#[test]
fn a_refusal_peeks_briefly_and_leaves_nothing_behind() {
    let mut feed = Feed::default();
    let refusal = feed.post(Kind::Refusal, "Nothing selected", None);
    assert_eq!(feed.peek().map(|entry| entry.id), Some(refusal));
    assert_eq!(feed.unread().count(), 0);
    assert!(feed.is_empty());
    assert!(feed.take_outbound().is_empty());
    assert!(feed.expire_peek(Instant::now() + REFUSAL_PEEK));
    assert!(feed.peek().is_none());
    assert!(feed.get(refusal).is_none());
}

#[test]
fn a_pending_approval_outlasts_the_limit() {
    let mut feed = Feed::default();
    let id = ApprovalId::new([7; 16]).unwrap();
    let approval = feed.post_approval("Approve kill?".into(), "agent".into(), id);
    for index in 0..LIMIT + 5 {
        feed.post(Kind::Info, &index.to_string(), None);
    }
    assert_eq!(feed.items().count(), LIMIT);
    assert_eq!(
        feed.get(approval).and_then(|entry| entry.approval),
        Some(id)
    );
    assert_eq!(
        feed.post_approval("again".into(), "agent".into(), id),
        approval
    );
    assert_eq!(
        feed.items()
            .filter(|entry| entry.kind == Kind::Info)
            .last()
            .map(|entry| entry.title.as_str()),
        Some("6")
    );
}

#[test]
fn the_oldest_entries_fall_off() {
    let mut feed = Feed::default();
    for index in 0..LIMIT + 5 {
        feed.post(Kind::Info, &index.to_string(), None);
    }
    assert_eq!(feed.items().count(), LIMIT);
    assert_eq!(
        feed.items().last().map(|entry| entry.title.as_str()),
        Some("5")
    );
}
