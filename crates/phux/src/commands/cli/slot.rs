//! `phux slot`: tabs inside one layout slot.

use crate::commands::SpawnSplit;
use usage::{Args, Subcommands};

/// Keep several panes in one layout slot, as tabs
///
/// A slot shows one of its tabs and keeps the others running at its size,
/// so switching never reflows. Attached clients switch what they show with
/// their own keys; `next` and `prev` here move the slot's default, what a
/// fresh attach and older clients show.
#[derive(Debug, Args)]
pub(crate) struct SlotArgs {
    #[usage(subcommand)]
    pub(crate) action: SlotAction,
}

/// `phux slot <action>`.
#[derive(Debug, Subcommands)]
pub(crate) enum SlotAction {
    /// Make PANE the last tab of the slot showing TO
    ///
    /// PANE leaves wherever it was: its own split collapses, or it leaves
    /// another slot.
    Add {
        /// The pane to make a tab.
        pane: String,
        /// Any tab of the slot it joins.
        #[usage(long, value_name = "PANE")]
        to: String,
        /// Emit a schema-versioned JSON result or error.
        #[usage(long)]
        json: bool,
    },
    /// Give the tab PANE a slot of its own, split beside the one it left
    Out {
        /// The tab to take out.
        pane: String,
        /// Split axis: `horizontal` stacks the panes, `vertical` places
        /// them side by side.
        #[usage(long, value_enum, default = "vertical")]
        split: SpawnSplit,
        /// Fraction left to the slot; strictly between 0 and 1.
        #[usage(
            long,
            default = "0.5",
            default_value_t = 0.5,
            validate = "float(value) > 0 && float(value) < 1",
            validate_error = "ratio must be finite and strictly between 0 and 1"
        )]
        ratio: f32,
        /// Emit a schema-versioned JSON result or error.
        #[usage(long)]
        json: bool,
    },
    /// Make the next tab the slot's default
    Next {
        /// Any tab of the slot; `.` for the focused pane.
        #[usage(default = ".")]
        pane: String,
        /// Emit a schema-versioned JSON result or error.
        #[usage(long)]
        json: bool,
    },
    /// Make the previous tab the slot's default
    Prev {
        /// Any tab of the slot; `.` for the focused pane.
        #[usage(default = ".")]
        pane: String,
        /// Emit a schema-versioned JSON result or error.
        #[usage(long)]
        json: bool,
    },
}
