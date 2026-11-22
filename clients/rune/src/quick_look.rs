//! Which file a path in terminal output names, for Quick Look (⌘Y).

use std::path::{Path, PathBuf};

use phux_client_runtime::publication::GridFrame;

/// libghostty's `CellWide::SpacerTail`: the right half of a wide glyph.
const WIDE_SPACER_TAIL: u8 = 2;

/// The run of non-blank characters around `col` in `row`: the word under
/// the pointer, as `ls` prints a file name.
pub fn word_at(frame: &GridFrame, col: u16, row: u16) -> String {
    let mut cells = Vec::new();
    for c in 0..frame.cols {
        let text = frame.cell_text(row, c);
        if text.is_empty() {
            let spacer = frame
                .cell(row, c)
                .is_some_and(|cell| cell.wide == WIDE_SPACER_TAIL);
            if !spacer {
                cells.push((' ', c));
            }
            continue;
        }
        cells.extend(String::from_utf8_lossy(text).chars().map(|ch| (ch, c)));
    }
    word(&cells, col)
}

fn word(cells: &[(char, u16)], col: u16) -> String {
    let Some(at) = cells
        .iter()
        .position(|(ch, c)| *c == col && !ch.is_whitespace())
    else {
        return String::new();
    };
    let blank = |(ch, _): &&(char, u16)| ch.is_whitespace();
    let start = at - cells[..at].iter().rev().take_while(|c| !blank(c)).count();
    let end = at + cells[at..].iter().take_while(|c| !blank(c)).count();
    cells[start..end].iter().map(|(ch, _)| ch).collect()
}

/// The existing file `token` names, relative to `cwd`. Quotes and brackets
/// around it go, and so do a trailing `:line:col` and sentence punctuation,
/// as compilers and prose print paths.
pub fn resolve(token: &str, cwd: Option<&Path>) -> Option<PathBuf> {
    let wrap = |c| matches!(c, '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '<' | '>');
    let mut token = token.trim();
    loop {
        token = token.trim_matches(wrap);
        if token.is_empty() {
            return None;
        }
        if let Some(path) = absolute(token, cwd).filter(|p| p.exists()) {
            return Some(path);
        }
        token = match token.strip_suffix(['.', ',', ';', ':']) {
            Some(shorter) => shorter,
            None => token.rsplit_once(':')?.0,
        };
    }
}

fn absolute(token: &str, cwd: Option<&Path>) -> Option<PathBuf> {
    let home = || std::env::var_os("HOME").map(PathBuf::from);
    if token == "~" {
        return home();
    }
    if let Some(rest) = token.strip_prefix("~/") {
        return Some(home()?.join(rest));
    }
    let path = Path::new(token);
    if path.is_absolute() {
        Some(path.to_owned())
    } else {
        Some(cwd?.join(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cells(line: &str) -> Vec<(char, u16)> {
        line.chars().zip(0..).collect()
    }

    #[test]
    fn the_word_under_the_column() {
        let line = cells("Cargo.toml  README.md  src");
        assert_eq!(word(&line, 14), "README.md");
        assert_eq!(word(&line, 0), "Cargo.toml");
        assert_eq!(word(&line, 25), "src");
        assert_eq!(word(&line, 11), "");
    }

    #[test]
    fn paths_resolve_against_the_cwd_and_shed_what_output_adds() {
        let dir = std::env::temp_dir().join(format!("phux-quick-look-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("README.md"), "hi").unwrap();
        std::fs::write(dir.join("src/main.rs"), "").unwrap();
        let cwd = Some(dir.as_path());
        assert_eq!(resolve("README.md", cwd), Some(dir.join("README.md")));
        assert_eq!(resolve("'README.md'.", cwd), Some(dir.join("README.md")));
        assert_eq!(
            resolve("src/main.rs:12:3:", cwd),
            Some(dir.join("src/main.rs"))
        );
        assert_eq!(resolve("(src/main.rs)", cwd), Some(dir.join("src/main.rs")));
        let absolute = dir.join("README.md").display().to_string();
        assert_eq!(resolve(&absolute, None), Some(dir.join("README.md")));
        assert_eq!(resolve("README.md", None), None);
        assert_eq!(resolve("missing.md", cwd), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
