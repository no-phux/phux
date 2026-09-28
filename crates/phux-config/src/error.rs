//! Error type for config parsing, with `line:col` location info.

use std::ops::Range;
use std::path::{Path, PathBuf};

/// Errors raised by [`crate::parse_str`] and related loaders.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// The TOML failed to parse or did not match the schema. `position` is
    /// `None` when the error had no span (a merged layer stack is not the
    /// user's text); a fabricated `1:1` would be worse than none.
    #[error("{}", parse_display(.path, *.position, .message))]
    Parse {
        /// Source path, used only for display.
        path: PathBuf,
        /// 1-indexed `(line, col)` of the offending token.
        position: Option<(usize, usize)>,
        /// Human-readable parse / deserialize message.
        message: String,
    },

    /// I/O failure reading the config file.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// A layer named by `extends` could not be read.
    #[error("{}: extends layer {}: {source}", referenced_from.display(), layer.display())]
    LayerRead {
        /// The layer file that failed to read.
        layer: PathBuf,
        /// The config file whose `extends` named the layer.
        referenced_from: PathBuf,
        /// The underlying read failure.
        source: std::io::Error,
    },

    /// An `extends` entry points back at a file already on the chain.
    #[error("{}: extends layer {} creates a cycle", referenced_from.display(), layer.display())]
    LayerCycle {
        /// The layer file that closed the cycle.
        layer: PathBuf,
        /// The config file whose `extends` named the layer.
        referenced_from: PathBuf,
    },

    /// A layer file violates the layering rules (ADR-0039): a bad `extends`,
    /// nesting past the depth cap, or `-append` misuse.
    #[error("{}: {message}", path.display())]
    Layer {
        /// The offending layer file.
        path: PathBuf,
        /// Human-readable description of the violation.
        message: String,
    },

    /// A settings edit was refused before anything was written.
    #[error("{key}: {message}")]
    Edit {
        /// The dotted key the edit targeted.
        key: String,
        /// Why the edit was refused.
        message: String,
    },

    /// The edited config could not be written to disk.
    #[error("{}: could not write: {source}", path.display())]
    Write {
        /// The config file that was being replaced.
        path: PathBuf,
        /// The underlying write or rename failure.
        source: std::io::Error,
    },
}

impl ConfigError {
    /// A [`ConfigError::Parse`] positioned at `span` within `input`.
    pub(crate) fn parse(
        path: &Path,
        input: &str,
        span: Option<Range<usize>>,
        message: impl Into<String>,
    ) -> Self {
        Self::Parse {
            path: path.to_path_buf(),
            position: span.map(|range| byte_offset_to_line_col(input, range.start)),
            message: message.into(),
        }
    }
}

/// `path: line:col: message`, or `path: message` without a position.
fn parse_display(path: &Path, position: Option<(usize, usize)>, message: &str) -> String {
    match position {
        Some((line, col)) => format!("{}: {line}:{col}: {message}", path.display()),
        None => format!("{}: {message}", path.display()),
    }
}

/// Convert a byte offset within `input` to a 1-indexed `(line, col)`,
/// counting columns in code points and clamping past the end.
#[must_use]
pub fn byte_offset_to_line_col(input: &str, offset: usize) -> (usize, usize) {
    let before = &input[..input.ceil_char_boundary(offset)];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    (line, col)
}

#[cfg(test)]
mod tests {
    use super::byte_offset_to_line_col;

    #[test]
    fn byte_offsets_map_to_one_indexed_code_point_positions() {
        for (input, offset, want) in [
            ("abc", 0, (1, 1)),
            ("abc", 2, (1, 3)),
            ("ab\ncd", 3, (2, 1)),
            ("ab\ncd", 4, (2, 2)),
            ("ab", 99, (1, 3)),
            ("é\nx", 2, (1, 2)),
            ("é\nx", 3, (2, 1)),
        ] {
            assert_eq!(
                byte_offset_to_line_col(input, offset),
                want,
                "{input:?}@{offset}"
            );
        }
    }
}
