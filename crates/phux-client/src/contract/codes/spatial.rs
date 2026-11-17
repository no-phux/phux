//! The refusal codes only the spatial edits (`insert-pane`, `move-pane`,
//! `swap-pane`) report.

/// `ratio` was not finite and strictly inside `(0, 1)`.
pub const INVALID_RATIO: &str = "invalid_ratio";
/// A selector matched no pane.
pub const SELECTOR_MISS: &str = "selector_miss";
/// Two selectors resolved to the same pane.
pub const SAME_PANE: &str = "same_pane";
/// The panes of a same-session edit live in different sessions.
pub const CROSS_SESSION: &str = "cross_session";
/// A resolved pane has no session in the snapshot.
pub const UNKNOWN_TERMINAL_SESSION: &str = "unknown_terminal_session";
/// The session has no persisted layout.
pub const LAYOUT_MISSING: &str = "layout_missing";
/// A selected pane is not in the persisted layout.
pub const PANE_NOT_IN_LAYOUT: &str = "pane_not_in_layout";
/// The pane being inserted is already in the layout.
pub const PANE_ALREADY_IN_LAYOUT: &str = "pane_already_in_layout";
/// The server refused the ownership move.
pub const MOVE_REFUSED: &str = "move_refused";
/// Ownership moved but the resulting state could not be read.
pub const POST_MOVE_STATE_FAILED: &str = "post_move_state_failed";
/// The destination changed windows during the move.
pub const DESTINATION_CHANGED: &str = "destination_changed";
/// The destination layout did not publish.
pub const DESTINATION_LAYOUT_FAILED: &str = "destination_layout_failed";
/// The source layout could not be cleaned up after the move.
pub const SOURCE_LAYOUT_FAILED: &str = "source_layout_failed";
/// A `projection` key did not name the addressed session.
pub const PROJECTION_INVALID: &str = "projection_invalid";
/// The wrong number of `projection` keys for the edit.
pub const PROJECTION_ARITY: &str = "projection_arity";
/// A slot tab edit named a pane that is no tab.
pub const NOT_A_TAB: &str = "not_a_tab";
/// The server does not keep slot tabs (`features_ext.SLOT_TABS`).
pub const SLOT_TABS_UNSUPPORTED: &str = "slot_tabs_unsupported";
