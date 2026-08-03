//! The single deprecation table: one row per absorbed spelling the binary
//! still accepts behind a hidden alias.
//!
//! Three consumers render or verify these rows, and none may drift from the
//! others (phux-i0e8.13.4):
//!
//! - `tests/deprecated_aliases.rs` runs every row against the real binary:
//!   the old argv must parse and print `note` exactly once on stderr, and
//!   the old spelling must be absent from `--help` and shell completions;
//! - `refdocs::deprecations` renders `docs/reference/deprecations.md` from
//!   the same rows, pinned by the refdocs freshness test;
//! - the clap-tree consistency test in `main.rs` walks the parser and
//!   asserts these rows are exactly the hidden deprecation surface — an
//!   unregistered hidden alias or a stale row fails there.
//!
//! This file is deliberately self-contained — no `crate::` paths, no
//! imports, no inline tests — because the binary crate exposes no library
//! target, so the integration test compiles this source file directly
//! (via `#[path]`) to read the same table the binary was compiled with.

/// Which kind of hidden surface carries an old spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeprecatedSurface {
    /// A hidden top-level verb (ADR-0066): the whole subcommand path is
    /// deprecated and dispatches through its visible replacement.
    Verb,
    /// A hidden boolean flag on a visible verb (phux-i0e8.8.4): the verb
    /// stays, one spelling of one axis is deprecated.
    Flag,
}

/// One deprecated spelling: what it was, what replaced it, the exact
/// stderr warning, an argv that exercises it, and its lifecycle releases.
// Some fields exist solely for the binary-level audit in
// `tests/deprecated_aliases.rs` (a separate crate that compiles this
// file via `#[path]`), so no build of the binary itself reads them.
#[allow(
    dead_code,
    reason = "consumed by the audit test that shares this table"
)]
pub(crate) struct Deprecation {
    /// The kind of hidden surface carrying the old spelling.
    pub(crate) surface: DeprecatedSurface,
    /// The old spelling as the user typed it, e.g. `phux remote add` or
    /// `phux insert-pane --horizontal`. For [`DeprecatedSurface::Verb`]
    /// rows this is `phux` followed by the canonical subcommand path; for
    /// [`DeprecatedSurface::Flag`] rows it ends with the deprecated long
    /// flag. The clap-tree consistency test matches on this shape.
    pub(crate) old: &'static str,
    /// The visible replacement spelling.
    pub(crate) new: &'static str,
    /// The exact one-line stderr warning the binary prints for the old
    /// spelling (suppressed under `--json` on the verb rows, where stdout
    /// carries only the document and stderr the one-line error contract).
    pub(crate) note: &'static str,
    /// Argv (after the binary name) run first to put a scratch config in
    /// the state the example needs. Empty for most rows.
    pub(crate) setup_argv: &'static [&'static str],
    /// Argv (after the binary name) proving the old spelling still parses
    /// and warns. Verb rows run to success without a server; flag rows are
    /// run against a dead socket and fail *after* the warning.
    pub(crate) example_argv: &'static [&'static str],
    /// Release that hid the spelling behind its replacement.
    pub(crate) deprecated_in: &'static str,
    /// Release scheduled to remove the spelling — the earliest release it
    /// can disappear in, after surviving at least one full release cycle
    /// with the warning in place.
    pub(crate) removed_in: &'static str,
}

#[allow(
    dead_code,
    reason = "consumed by the audit test that shares this table"
)]
impl Deprecation {
    /// The subcommand words of the old spelling: `"phux remote add"` yields
    /// `["remote", "add"]`; flag rows yield just the carrying verb.
    pub(crate) fn old_verb_path(&self) -> Vec<&'static str> {
        self.old
            .split_whitespace()
            .skip(1)
            .take_while(|word| !word.starts_with("--"))
            .collect()
    }

    /// The deprecated long flag of a [`DeprecatedSurface::Flag`] row
    /// (`Some("--horizontal")`); `None` on verb rows.
    pub(crate) fn old_flag(&self) -> Option<&'static str> {
        self.old
            .split_whitespace()
            .next_back()
            .filter(|word| word.starts_with("--"))
    }
}

/// Every deprecated spelling the binary currently accepts, in page order:
/// the ADR-0066 machine-registry verbs absorbed into `phux host`, then the
/// split booleans absorbed into `--split` (phux-i0e8.8.4).
pub(crate) const DEPRECATED: &[Deprecation] = &[
    Deprecation {
        surface: DeprecatedSurface::Verb,
        old: "phux remote add",
        new: "phux host add",
        note: "phux: `phux remote add` is deprecated and will be removed; use `phux host add`",
        setup_argv: &[],
        example_argv: &["remote", "add", "mini", "ssh://mini"],
        deprecated_in: "v0.10.0",
        removed_in: "v0.12.0",
    },
    Deprecation {
        surface: DeprecatedSurface::Verb,
        old: "phux remote list",
        new: "phux host ls",
        note: "phux: `phux remote list` is deprecated and will be removed; use `phux host ls`",
        setup_argv: &[],
        example_argv: &["remote", "list"],
        deprecated_in: "v0.10.0",
        removed_in: "v0.12.0",
    },
    Deprecation {
        surface: DeprecatedSurface::Verb,
        old: "phux remote remove",
        new: "phux host rm",
        note: "phux: `phux remote remove` is deprecated and will be removed; use `phux host rm`",
        setup_argv: &["host", "add", "mini", "ssh://mini"],
        example_argv: &["remote", "remove", "mini"],
        deprecated_in: "v0.10.0",
        removed_in: "v0.12.0",
    },
    Deprecation {
        surface: DeprecatedSurface::Verb,
        old: "phux satellite add",
        new: "phux host add --role satellite",
        note: "phux: `phux satellite add` is deprecated and will be removed; \
               use `phux host add --role satellite`",
        setup_argv: &[],
        example_argv: &["satellite", "add", "edge", "ssh://edge"],
        deprecated_in: "v0.10.0",
        removed_in: "v0.12.0",
    },
    Deprecation {
        surface: DeprecatedSurface::Verb,
        old: "phux satellite list",
        new: "phux host ls --role satellite",
        note: "phux: `phux satellite list` is deprecated and will be removed; \
               use `phux host ls --role satellite`",
        setup_argv: &[],
        example_argv: &["satellite", "list"],
        deprecated_in: "v0.10.0",
        removed_in: "v0.12.0",
    },
    Deprecation {
        surface: DeprecatedSurface::Verb,
        old: "phux satellite enroll",
        new: "phux host enroll --role satellite",
        note: "phux: `phux satellite enroll` is deprecated and will be removed; \
               use `phux host enroll --role satellite`",
        setup_argv: &[],
        example_argv: &["satellite", "enroll", "edge", "--ssh-only"],
        deprecated_in: "v0.10.0",
        removed_in: "v0.12.0",
    },
    Deprecation {
        surface: DeprecatedSurface::Verb,
        old: "phux satellite remove",
        new: "phux host rm --role satellite",
        note: "phux: `phux satellite remove` is deprecated and will be removed; \
               use `phux host rm --role satellite`",
        setup_argv: &["host", "add", "edge", "ssh://edge", "--role", "satellite"],
        example_argv: &["satellite", "remove", "edge"],
        deprecated_in: "v0.10.0",
        removed_in: "v0.12.0",
    },
    Deprecation {
        surface: DeprecatedSurface::Verb,
        old: "phux enroll",
        new: "phux host enroll",
        note: "phux: `phux enroll` is deprecated and will be removed; use `phux host enroll`",
        setup_argv: &[],
        example_argv: &["enroll", "me@mini", "--ssh-only"],
        deprecated_in: "v0.10.0",
        removed_in: "v0.12.0",
    },
    Deprecation {
        surface: DeprecatedSurface::Flag,
        old: "phux insert-pane --horizontal",
        new: "phux insert-pane --split horizontal",
        note: "phux: --horizontal is deprecated and will be removed; \
               use `--split horizontal` (or `--split h`)",
        setup_argv: &[],
        example_argv: &["insert-pane", "%1", "%2", "--horizontal"],
        deprecated_in: "v0.9.0",
        removed_in: "v0.12.0",
    },
    Deprecation {
        surface: DeprecatedSurface::Flag,
        old: "phux insert-pane --vertical",
        new: "phux insert-pane --split vertical",
        note: "phux: --vertical is deprecated and will be removed; \
               use `--split vertical` (or `--split v`)",
        setup_argv: &[],
        example_argv: &["insert-pane", "%1", "%2", "--vertical"],
        deprecated_in: "v0.9.0",
        removed_in: "v0.12.0",
    },
    Deprecation {
        surface: DeprecatedSurface::Flag,
        old: "phux move-pane --horizontal",
        new: "phux move-pane --split horizontal",
        note: "phux: --horizontal is deprecated and will be removed; \
               use `--split horizontal` (or `--split h`)",
        setup_argv: &[],
        example_argv: &["move-pane", "%1", "%2", "--horizontal"],
        deprecated_in: "v0.9.0",
        removed_in: "v0.12.0",
    },
    Deprecation {
        surface: DeprecatedSurface::Flag,
        old: "phux move-pane --vertical",
        new: "phux move-pane --split vertical",
        note: "phux: --vertical is deprecated and will be removed; \
               use `--split vertical` (or `--split v`)",
        setup_argv: &[],
        example_argv: &["move-pane", "%1", "%2", "--vertical"],
        deprecated_in: "v0.9.0",
        removed_in: "v0.12.0",
    },
];
