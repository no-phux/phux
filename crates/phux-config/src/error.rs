//! Error type for config parsing, with `line:col` location info.

use std::path::PathBuf;

/// Errors raised by [`crate::parse_str`] and related loaders.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// The TOML failed to parse or did not match the schema.
    ///
    /// `position` is the 1-indexed `(line, col)` of the start of the
    /// offending token, when the underlying error carried a span.
    /// Deserialize errors on a merged layer stack carry none — the
    /// value being deserialized is not the user's text — and those
    /// render as `path: message` with no position at all. Reporting a
    /// confident, fabricated `1:1` is worse than reporting none
    /// (phux-i0e8.3.5).
    #[error("{}", parse_display(.path, *.position, .message))]
    Parse {
        /// Source path, used only for display.
        path: PathBuf,
        /// 1-indexed `(line, col)`; `None` when the error had no span.
        position: Option<(usize, usize)>,
        /// Human-readable parse / deserialize message.
        message: String,
    },

    /// I/O failure reading the config file.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// A layer named by `extends` could not be read (missing file,
    /// permission failure, ...). Names both the layer and the file
    /// that referenced it.
    #[error("{}: extends layer {}: {source}", referenced_from.display(), layer.display())]
    LayerRead {
        /// The layer file that failed to read.
        layer: PathBuf,
        /// The config file whose `extends` named the layer.
        referenced_from: PathBuf,
        /// The underlying read failure.
        source: std::io::Error,
    },

    /// An `extends` entry points back at a file already on the current
    /// resolution chain.
    #[error("{}: extends layer {} creates a cycle", referenced_from.display(), layer.display())]
    LayerCycle {
        /// The layer file that closed the cycle.
        layer: PathBuf,
        /// The config file whose `extends` named the layer.
        referenced_from: PathBuf,
    },

    /// A layer file violates the layering rules (ADR-0039): a bad
    /// `extends` value, nesting past the depth cap, or `-append`
    /// misuse. `path` is the offending layer file.
    #[error("{}: {message}", path.display())]
    Layer {
        /// The layer file the rule violation was found in.
        path: PathBuf,
        /// Human-readable description of the violation.
        message: String,
    },
}

/// Render the [`ConfigError::Parse`] display line: `path: line:col:
/// message` when a position is known, `path: message` when it is not.
fn parse_display(
    path: &std::path::Path,
    position: Option<(usize, usize)>,
    message: &str,
) -> String {
    match position {
        Some((line, col)) => format!("{}: {line}:{col}: {message}", path.display()),
        None => format!("{}: {message}", path.display()),
    }
}

/// Convert a byte offset within `input` to a 1-indexed `(line, col)`.
///
/// Columns count UTF-8 *code points*, not grapheme clusters — adequate
/// for pointing diagnostics at ASCII config keys, which is the
/// overwhelming case for TOML.
///
/// If `offset` lies beyond the end of `input`, the result clamps to
/// the last position.
#[must_use]
pub fn byte_offset_to_line_col(input: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(input.len());
    let mut line = 1usize;
    let mut col = 1usize;
    let mut idx = 0usize;
    for ch in input.chars() {
        if idx >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
        idx += ch.len_utf8();
    }
    (line, col)
}

#[cfg(test)]
mod tests {
    use super::byte_offset_to_line_col;

    #[test]
    fn start_is_one_one() {
        assert_eq!(byte_offset_to_line_col("abc", 0), (1, 1));
    }

    #[test]
    fn advances_columns() {
        assert_eq!(byte_offset_to_line_col("abc", 2), (1, 3));
    }

    #[test]
    fn newline_resets_column() {
        assert_eq!(byte_offset_to_line_col("ab\ncd", 3), (2, 1));
        assert_eq!(byte_offset_to_line_col("ab\ncd", 4), (2, 2));
    }

    #[test]
    fn offset_past_end_clamps() {
        assert_eq!(byte_offset_to_line_col("ab", 99), (1, 3));
    }

    #[test]
    fn multibyte_counts_codepoints() {
        // "é" is two bytes in UTF-8 but one column.
        let s = "é\nx";
        // offset 0 → 1:1; offset 2 (after é) → 1:2; offset 3 (after \n) → 2:1
        assert_eq!(byte_offset_to_line_col(s, 0), (1, 1));
        assert_eq!(byte_offset_to_line_col(s, 2), (1, 2));
        assert_eq!(byte_offset_to_line_col(s, 3), (2, 1));
    }
}
