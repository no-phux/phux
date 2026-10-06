//! Size budget for the attach client (phux-jx39.7): the parameter-threading
//! and god-function debt the jx39 stages paid down cannot silently come back.
//!
//! Clippy already fails any function over 7 parameters or 100 lines; the only
//! way past it is an `#[allow]`. So the budget pins the allows themselves:
//! each surviving `too_many_arguments` / `too_many_lines` allow is listed
//! here with its ceiling, and a new allow, a grown signature, or a driver file
//! past its line budget fails this test. Lower a number when you pay debt
//! down; raise one only deliberately, with the reason in the same change.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::fs;
use std::path::{Path, PathBuf};

/// `(file, fn, max params)`: the functions still allowed past clippy's
/// 7-parameter threshold, and how far past it.
const TOO_MANY_ARGUMENTS: &[(&str, &str, usize)] = &[
    // The per-entry contract with `entry.rs`; folded into `SessionLoop` on
    // its first statement.
    ("attach/driver/main_loop.rs", "main_loop", 14),
    // The per-invocation knobs of the `run_*` entry points.
    ("attach/driver/entry.rs", "attach_session", 10),
    // The row-diff hot path; attach/render.rs is outside the jx39 scope.
    ("attach/render.rs", "paint_dirty_rows", 10),
];

/// `(file, fn)`: the functions still allowed past clippy's 100-line limit.
const TOO_MANY_LINES: &[(&str, &str)] = &[("attach/driver/loop_state.rs", "new")];

/// `(file, max lines)` for the driver files the jx39 epic shrank.
const FILE_LINES: &[(&str, usize)] = &[
    ("attach/driver/main_loop.rs", 200),
    // 3650 at the restored jx39 guard. #1010 (per-tile cell pixels) and
    // #1031 (dropping terminal-reply plumbing) landed the file at 3653.
    ("attach/driver/loop_state.rs", 3653),
    // 2250 at jx39.7; 7a5db74ee (history-unavailable badge) and 8fdaca2ea
    // (overlay plugin panes) grew it while this guard was accidentally
    // deleted (248598c2b), so the budget pins the shipped size.
    ("attach/server_frame/handler.rs", 2320),
];

const RULE: &str = "\
Budget exceeded. Before raising a number here, give the state an owner
instead of another parameter: the frame dispatcher reads a SessionMirror and
a FrameEnv, the paint layer a ChromeCtx, the peer sweeps a PeerWatch, the
dispatcher a DispatchCtx. Raise a budget only deliberately, with the reason
in the same change.";

fn src_root() -> PathBuf {
    // Read at run time, not `env!`: see scripts/check-cache-portable.sh.
    PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("the test runner sets CARGO_MANIFEST_DIR"),
    )
    .join("src")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read src dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// Every source file as `(path relative to src, lines with `//` comments
/// blanked)`.
fn sources() -> Vec<(String, Vec<String>)> {
    let root = src_root();
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    assert!(
        files.len() > 20,
        "source walk found only {} files; the scan is not looking where it thinks it is",
        files.len()
    );
    files
        .into_iter()
        .map(|path| {
            let rel = path
                .strip_prefix(&root)
                .expect("path under src")
                .to_string_lossy()
                .replace('\\', "/");
            let body = fs::read_to_string(&path).expect("read source file");
            let lines = body
                .lines()
                .map(|line| line.split("//").next().unwrap_or("").to_owned())
                .collect();
            (rel, lines)
        })
        .collect()
}

/// The first `fn` item at or after `from`: its name and its parameter count
/// (`self` included, as clippy counts it).
fn next_fn(lines: &[String], from: usize) -> (String, usize) {
    let start = (from..lines.len())
        .find(|&idx| is_fn_item(&lines[idx]))
        .expect("an allow attribute is followed by a fn");
    let signature: String = lines[start..].join("\n");
    let after_fn = &signature[signature.find("fn ").expect("fn keyword") + 3..];
    let name: String = after_fn
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    let open = after_fn.find('(').expect("parameter list");
    (name, count_params(&after_fn[open + 1..]))
}

/// Whether `line` opens a `fn` item: only a visibility and `async`/`const`/
/// `unsafe`/`extern` may precede the keyword (so an attribute's reason text
/// that mentions "fn" is not one).
fn is_fn_item(line: &str) -> bool {
    let line = line.trim_start();
    let Some(at) = line.find("fn ") else {
        return false;
    };
    let mut before = &line[..at];
    if let Some(rest) = before.strip_prefix("pub(") {
        before = rest.split_once(')').map_or("", |(_, tail)| tail);
    }
    before
        .split_whitespace()
        .all(|word| matches!(word, "pub" | "async" | "const" | "unsafe" | "extern"))
}

/// Top-level commas up to the closing paren of a parameter list, ignoring the
/// `>` of a `->` inside a closure bound.
fn count_params(list: &str) -> usize {
    let (mut depth, mut params, mut pending, mut prev) = (0_i32, 0, false, ' ');
    for c in list.chars() {
        match c {
            '(' | '[' | '{' | '<' => depth += 1,
            '>' if prev == '-' => {}
            ')' if depth == 0 => break,
            ')' | ']' | '}' | '>' => depth -= 1,
            ',' if depth == 0 => {
                params += usize::from(pending);
                pending = false;
                prev = c;
                continue;
            }
            _ => {}
        }
        pending |= !c.is_whitespace();
        prev = c;
    }
    params + usize::from(pending)
}

/// Every `(file, fn, params)` carrying an allow for `lint`.
fn allowed(sources: &[(String, Vec<String>)], lint: &str) -> Vec<(String, String, usize)> {
    let mut found = Vec::new();
    for (rel, lines) in sources {
        for (idx, line) in lines.iter().enumerate() {
            if line.contains(lint) {
                let (name, params) = next_fn(lines, idx + 1);
                found.push((rel.clone(), name, params));
            }
        }
    }
    found
}

#[test]
fn too_many_arguments_allows_stay_within_budget() {
    let sources = sources();
    let mut offenders = Vec::new();
    for (file, name, params) in allowed(&sources, "clippy::too_many_arguments") {
        match TOO_MANY_ARGUMENTS
            .iter()
            .find(|(f, n, _)| *f == file && *n == name)
        {
            None => offenders.push(format!("{file}: new allow on `{name}` ({params} params)")),
            Some((_, _, max)) if params > *max => {
                offenders.push(format!(
                    "{file}: `{name}` grew to {params} params (budget {max})"
                ));
            }
            Some(_) => {}
        }
    }
    assert!(
        offenders.is_empty(),
        "{RULE}\n\n  {}",
        offenders.join("\n  ")
    );
}

#[test]
fn too_many_lines_allows_stay_within_budget() {
    let sources = sources();
    let offenders: Vec<String> = allowed(&sources, "clippy::too_many_lines")
        .into_iter()
        .filter(|(file, name, _)| !TOO_MANY_LINES.contains(&(file.as_str(), name.as_str())))
        .map(|(file, name, _)| format!("{file}: new too_many_lines allow on `{name}`"))
        .collect();
    assert!(
        offenders.is_empty(),
        "{RULE}\n\n  {}",
        offenders.join("\n  ")
    );
}

#[test]
fn driver_files_stay_within_their_line_budget() {
    let root = src_root();
    let offenders: Vec<String> = FILE_LINES
        .iter()
        .filter_map(|(file, max)| {
            let lines = fs::read_to_string(root.join(file))
                .expect("budgeted file exists")
                .lines()
                .count();
            (lines > *max).then(|| format!("{file}: {lines} lines (budget {max})"))
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "{RULE}\n\n  {}",
        offenders.join("\n  ")
    );
}

#[test]
fn the_param_counter_counts_what_clippy_counts() {
    assert!(is_fn_item("    pub(in crate::attach) async fn go<W>("));
    assert!(is_fn_item("const fn go() {"));
    assert!(!is_fn_item(
        r#"    reason = "a builder for one internal fn would be ceremony""#
    ));
    assert_eq!(count_params(") -> u8"), 0);
    assert_eq!(count_params("&mut self) -> u8"), 1);
    assert_eq!(
        count_params("a: u8, b: Vec<(u8, u8)>,\n c: impl Fn(u8) -> u8,\n)"),
        3
    );
    assert_eq!(count_params("m: HashMap<u32, String>, f: [u8; 4])"), 2);
}
