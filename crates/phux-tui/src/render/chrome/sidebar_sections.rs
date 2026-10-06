//! Plugin sidebar sections (ADR-0148): titled, fixed-height panels a plugin
//! manifest's `[[sidebar]]` entry declares, laid between Agents and Sessions.
//!
//! A section's height is its header plus its declared `rows`, whatever its
//! population, so a section filling or emptying never moves Sessions
//! (ADR-0112). Sections are laid only while Agents and Sessions keep
//! `MIN_CORE_ROWS` body rows between them; the last declared section yields
//! first. The rows themselves are projected by the attach layer from pane
//! state the client already holds; this module only shapes and names them.

/// Most plugin sections one strip lays out; further declarations are
/// dropped with a warning when the settings are built.
pub const MAX_PLUGIN_SECTIONS: usize = 4;

/// Body rows Agents and Sessions keep between them before any plugin
/// section is laid (a header and three rows each).
pub(super) const MIN_CORE_ROWS: usize = 8;

/// One declared section, as the painter keeps it between projections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSectionSpec {
    /// Header text.
    pub title: String,
    /// Row template with `{token}` placeholders (validated at manifest load).
    pub format: String,
    /// Rows reserved under the header.
    pub rows: u8,
}

/// One projected row: its rendered text and the pane a click focuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSectionRow {
    /// The section's `format` with every token resolved for this pane.
    pub text: String,
    /// `select-window` index of the window holding the pane.
    pub window: usize,
    /// The pane's DFS leaf ordinal in that window (`focus-pane`'s `pane`).
    pub pane: usize,
}

/// One section with its projected rows, in session window/leaf order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSection {
    /// Header text.
    pub title: String,
    /// Rows reserved under the header.
    pub rows: u8,
    /// Every pane that resolved all of the section's tokens.
    pub entries: Vec<PluginSectionRow>,
}

/// The sections' shape as the row model needs it: `Copy`, so it rides in
/// `SidebarCounts` and a click resolves against the shape it landed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PluginShape {
    len: usize,
    /// `(reserved rows, population)` per section.
    sections: [(u8, usize); MAX_PLUGIN_SECTIONS],
}

impl PluginShape {
    /// The shape of `sections`, keeping at most [`MAX_PLUGIN_SECTIONS`].
    #[must_use]
    pub fn of(sections: &[PluginSection]) -> Self {
        let mut shape = Self::default();
        for section in sections.iter().take(MAX_PLUGIN_SECTIONS) {
            shape.sections[shape.len] = (section.rows, section.entries.len());
            shape.len += 1;
        }
        shape
    }

    /// The `(reserved rows, population)` of each section, in order.
    pub fn sections(&self) -> impl Iterator<Item = (u8, usize)> + '_ {
        self.sections[..self.len].iter().copied()
    }

    /// How many leading sections fit a `body`-row strip while Agents and
    /// Sessions keep `MIN_CORE_ROWS`.
    #[must_use]
    pub fn fitting(&self, body: usize) -> usize {
        let room = body.saturating_sub(MIN_CORE_ROWS);
        let mut used = 0;
        let mut fit = 0;
        for (rows, _) in self.sections() {
            used += 1 + usize::from(rows);
            if used > room {
                break;
            }
            fit += 1;
        }
        fit
    }

    /// Rows the first `n` sections occupy (headers included).
    #[must_use]
    pub fn height(&self, n: usize) -> usize {
        self.sections()
            .take(n)
            .map(|(rows, _)| 1 + usize::from(rows))
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(rows: u8, population: usize) -> PluginSection {
        PluginSection {
            title: "S".to_owned(),
            rows,
            entries: (0..population)
                .map(|i| PluginSectionRow {
                    text: format!("row {i}"),
                    window: 0,
                    pane: i,
                })
                .collect(),
        }
    }

    #[test]
    fn shape_caps_sections_and_fits_whole_sections_only() {
        let sections: Vec<_> = (0..6).map(|_| section(3, 1)).collect();
        let shape = PluginShape::of(&sections);
        assert_eq!(shape.sections().count(), MAX_PLUGIN_SECTIONS);
        // Each section is 4 rows; the core keeps 8.
        assert_eq!(shape.fitting(8), 0);
        assert_eq!(shape.fitting(11), 0);
        assert_eq!(shape.fitting(12), 1);
        assert_eq!(shape.fitting(19), 2);
        assert_eq!(shape.fitting(100), MAX_PLUGIN_SECTIONS);
        assert_eq!(shape.height(2), 8);
    }

    #[test]
    fn population_never_changes_the_fit() {
        let empty = PluginShape::of(&[section(2, 0)]);
        let full = PluginShape::of(&[section(2, 9)]);
        for body in 0..20 {
            assert_eq!(empty.fitting(body), full.fitting(body));
            assert_eq!(empty.height(1), full.height(1));
        }
    }
}
