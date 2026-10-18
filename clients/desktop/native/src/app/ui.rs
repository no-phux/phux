//! The desktop's component library: small, stateless building blocks every
//! surface composes, plus the one glass menu.
//!
//! Nothing here reads a [`super::desk::Desk`]. A component takes the paint it
//! needs as a [`Look`] and its behavior as closures, so the titlebar, the
//! navigator, panes, overlays and settings all draw the same row, badge, button
//! and menu instead of restyling their own.
//!
//! | Module | Owns |
//! |---|---|
//! | [`atoms`] | text tones, dots, badges, shadows and highlights |
//! | [`buttons`] | ghost, icon and titlebar buttons |
//! | [`list`] | navigator and list rows, their icon slot, section labels |
//! | [`status`] | agent state colours and the attention mark overlaid on rows and tabs |
//! | [`anchor`] | where a trigger was painted, so a menu can drop from it |
//! | [`menu`] | the glass menu: native material, pages, keyboard, springs |
//! | [`segmented`] | a few exclusive options in one track |
//! | [`time`] | relative ages and when their labels next change |

pub(crate) mod anchor;
pub(crate) mod atoms;
pub(crate) mod buttons;
pub(crate) mod list;
pub(crate) mod menu;
pub(crate) mod segmented;
pub(crate) mod status;
pub(crate) mod time;

pub(crate) use atoms::{badge, dot, keystroke, muted, notice_card, pill, shadow, specular};
pub(crate) use buttons::{ghost_button, ghost_icon, icon_button};
pub(crate) use list::{row, row_icon, section_label};
pub(crate) use status::{agent_swatch, attention_mark};
pub(crate) use time::{ago, ago_wait};
