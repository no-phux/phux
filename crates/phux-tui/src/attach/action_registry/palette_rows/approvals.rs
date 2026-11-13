//! Approval review's palette entry; decisions are hidden, explicit actions.
use super::{ActionSpec, Category, Palette};

pub(super) const REVIEW: ActionSpec = ActionSpec {
    name: "approvals",
    description: "Review pending credential requests and held actions on the attached server",
    params: "",
    palette: Palette::Row {
        title: "Approvals…",
        category: Category::Session,
        args: &[],
    },
    run: crate::attach::input_dispatch::approvals::queue,
};
