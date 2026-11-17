//! The cases the inbox is drawn in: what each puts on the desk and what
//! it checks once drawn.

use rune::{Key, Runtime};

use super::{ANSWERS, WORDS, approval, delivery, question, settle};
use crate::desk::Desk;

#[derive(Clone, Copy)]
pub(super) enum Scene {
    Question,
    Reply,
    Approval,
    Empty,
    Scrolled,
    NativeReply,
    Inline,
    InlineNoChoices,
    Sending,
    Delivered,
    Refused,
    Unknown,
    NewDraft,
    OtherQuestion,
}

impl Scene {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Question => "question",
            Self::Reply => "reply",
            Self::Approval => "approval",
            Self::Empty => "empty",
            Self::Scrolled => "scrolled",
            Self::NativeReply => "native-reply",
            Self::Inline => "inline",
            Self::InlineNoChoices => "inline-no-choices",
            Self::Sending => "sending",
            Self::Delivered => "delivered",
            Self::Refused => "refused",
            Self::Unknown => "unknown",
            Self::NewDraft => "new-draft",
            Self::OtherQuestion => "other-question",
        }
    }

    pub(super) fn populate(self, desk: &mut Desk) {
        match self {
            Self::Empty => {}
            Self::Approval | Self::Scrolled => {
                question(desk, &ANSWERS);
                desk.inbox.approvals.push(approval());
                if matches!(self, Self::Scrolled) {
                    for index in 1..6 {
                        let mut approval = approval();
                        approval.id = format!("{index:032x}");
                        desk.inbox.approvals.push(approval);
                    }
                }
            }
            Self::Question | Self::Reply | Self::NativeReply | Self::Inline => {
                question(desk, &ANSWERS)
            }
            Self::InlineNoChoices => question(desk, &[]),
            Self::Sending
            | Self::Delivered
            | Self::Refused
            | Self::Unknown
            | Self::NewDraft
            | Self::OtherQuestion => question(desk, &ANSWERS),
        }
        if matches!(self, Self::Inline | Self::InlineNoChoices) {
            desk.phone.as_mut().unwrap().keyboard = matches!(self, Self::Inline);
            return;
        }
        if matches!(self, Self::NativeReply) {
            desk.native = Some(crate::mobile::Platform::test());
        }
        desk.toggle_inbox();
        desk.inbox.reply.focused = false;
        if let Some(phone) = desk.phone.as_mut() {
            phone.keyboard = !matches!(
                self,
                Self::Question | Self::Approval | Self::Empty | Self::Scrolled | Self::NativeReply
            );
        }
    }

    pub(super) fn check(self, rt: &mut Runtime<Desk>) {
        match self {
            Self::Reply | Self::NativeReply => {
                assert!(rt.accessibility_action(
                    "inbox-reply",
                    rune::accesskit::Action::Focus,
                    None
                ));
                rt.key(crate::keys::plain(Key::Text(
                    "Please keep the existing pairing and run the checks.".into(),
                )));
                settle(rt);
                assert!(rt.state.inbox.reply.text.contains("existing pairing"));
            }
            Self::Scrolled => {
                let key = format!(
                    "chip-{}-Wait",
                    rt.state.connection.shown.as_ref().unwrap().resource
                );
                rt.update(|_, tx| tx.scroll_into_view(&key));
                settle(rt);
                let [_, y, _, h] = rt.presented(&key).unwrap().0;
                assert!(y >= 0.0 && y + h <= rt.viewport.1);
                assert_eq!(rt.state.inbox_items().len(), 7);
            }
            Self::Inline | Self::InlineNoChoices => {
                let id = rt.state.connection.shown.as_ref().unwrap().resource.clone();
                assert!(
                    rt.find(&format!("ask-{id}")).is_some(),
                    "actual pane owns the inline question"
                );
            }
            Self::Question => {
                assert!(
                    rt.elements()
                        .iter()
                        .any(|e| e.text.as_deref() == Some(WORDS))
                );
                assert_eq!(rt.state.inbox_items().len(), 1);
            }
            Self::Approval => {
                assert_eq!(rt.state.inbox_items().len(), 2);
                assert!(
                    rt.accessibility_json()
                        .to_string()
                        .contains("Signal terminal")
                );
            }
            Self::Empty => {
                assert!(rt.state.inbox_items().is_empty());
                assert!(
                    rt.accessibility_json()
                        .to_string()
                        .contains("Nothing needs you")
                );
            }
            Self::Sending
            | Self::Delivered
            | Self::Refused
            | Self::Unknown
            | Self::NewDraft
            | Self::OtherQuestion => delivery::prepare(rt, self),
        }
    }
}
