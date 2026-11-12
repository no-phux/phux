//! Setting what restyles a pane's cells at paint time: the copy-mode marks
//! and the unfocused-pane dim.

use super::{CopyMarks, SelectionRect, TerminalRenderer};

impl TerminalRenderer<'_> {
    /// Set (or clear) the copy-mode selection for the next render. A change
    /// forgets the front buffer.
    pub fn set_selection(&mut self, selection: Option<SelectionRect>) {
        self.set_copy_marks(CopyMarks::selection(selection));
    }

    /// Set the copy-mode selection and search hits for the next render,
    /// keeping the pane's dim. A change forgets the front buffer.
    pub fn set_copy_marks(&mut self, marks: CopyMarks) {
        let marks = CopyMarks {
            dim: self.marks.dim,
            ..marks
        };
        if self.marks != marks {
            self.front.invalidate_all();
        }
        self.marks = marks;
    }

    /// Paint the pane faint (or not) from the next render on. Returns whether
    /// that changed, in which case the front buffer is forgotten.
    pub fn set_dim(&mut self, dim: bool) -> bool {
        let changed = self.marks.dim != dim;
        if changed {
            self.marks.dim = dim;
            self.front.invalidate_all();
        }
        changed
    }
}
