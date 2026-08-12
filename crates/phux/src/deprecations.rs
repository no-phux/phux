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
//! - the clap-tree consistency test in `lib.rs` walks the parser and
//!   asserts these rows are exactly the hidden deprecation surface — an
//!   unregistered hidden alias or a stale row fails there.
//!
//! This file is deliberately self-contained — no `crate::` paths, no
//! imports, no inline tests — because this module and its items are
//! `pub(crate)`, not part of the crate's public API, so an integration
//! test (a separate crate) cannot reach them through the ordinary `phux::`
//! path. `tests/deprecated_aliases.rs` instead compiles this source file
//! directly (via `#[path]`) to read the same table the binary was compiled
//! with.

/// Which kind of hidden surface carries an old spelling.
// No variant is constructed while `DEPRECATED` is empty; both stay live for
// the next row (a hidden verb or a hidden flag) that needs one.
#[allow(dead_code, reason = "constructed by the next row added to DEPRECATED")]
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

/// Every deprecated spelling the binary currently accepts.
///
/// Empty: the ADR-0066 machine-registry verbs (`remote`, `satellite`,
/// top-level `enroll`) and the split booleans (`--horizontal`/`--vertical`,
/// phux-i0e8.8.4) that once lived here were removed in v0.12.1, once their
/// `removed_in` release shipped. The table, and every consumer that reads
/// it (the audit in `tests/deprecated_aliases.rs`, the generated
/// `docs/reference/deprecations.md`, and the clap-tree pin test in
/// `lib.rs`), stay wired up for the next spelling that needs one release
/// cycle of warning before it goes.
pub(crate) const DEPRECATED: &[Deprecation] = &[];

/// One spelling that is gone: what it was, what replaced it, and when it
/// went.
///
/// A removed spelling no longer parses, so clap answers it with a generic
/// nearest-match — which is actively misleading here, because the nearest
/// string is rarely the right migration (`phux remote add` resolved to
/// "a similar subcommand exists: `rename`"). These rows let the parse
/// error name the actual replacement for a release or two after removal,
/// after which the row ages out and clap's ordinary message is the honest
/// answer.
pub(crate) struct Removal {
    /// The kind of surface the old spelling was.
    pub(crate) surface: DeprecatedSurface,
    /// The old spelling as the user typed it, in the same shape as
    /// [`Deprecation::old`]: `phux remote add`, or a verb followed by the
    /// removed long flag.
    pub(crate) old: &'static str,
    /// The visible replacement spelling.
    pub(crate) new: &'static str,
    /// The release the spelling stopped parsing in.
    pub(crate) removed_in: &'static str,
}

impl Removal {
    /// The first subcommand word of a [`DeprecatedSurface::Verb`] row
    /// (`"phux remote add"` yields `Some("remote")`); `None` on flag rows.
    /// This is what clap reports back as the invalid subcommand.
    pub(crate) fn old_root_verb(&self) -> Option<&'static str> {
        match self.surface {
            DeprecatedSurface::Verb => self.old.split_whitespace().nth(1),
            DeprecatedSurface::Flag => None,
        }
    }

    /// The removed long flag of a [`DeprecatedSurface::Flag`] row
    /// (`Some("--horizontal")`); `None` on verb rows.
    pub(crate) fn old_flag(&self) -> Option<&'static str> {
        self.old
            .split_whitespace()
            .next_back()
            .filter(|word| word.starts_with("--"))
    }

    /// The verb a [`DeprecatedSurface::Flag`] row's flag hung off
    /// (`"phux insert-pane --horizontal"` yields `Some("insert-pane")`).
    pub(crate) fn flag_verb(&self) -> Option<&'static str> {
        match self.surface {
            DeprecatedSurface::Flag => self.old.split_whitespace().nth(1),
            DeprecatedSurface::Verb => None,
        }
    }
}

/// Every spelling removed recently enough that naming its replacement in a
/// parse error still helps someone upgrading.
///
/// Rows are dropped once the release that removed them is far enough back
/// that anyone still typing the old spelling is not mid-upgrade — at which
/// point clap's generic message is the honest answer and this table should
/// shrink rather than accumulate.
pub(crate) const REMOVED: &[Removal] = &[
    Removal {
        surface: DeprecatedSurface::Verb,
        old: "phux remote",
        new: "phux host",
        removed_in: "v0.12.1",
    },
    Removal {
        surface: DeprecatedSurface::Verb,
        old: "phux satellite",
        new: "phux host --role satellite",
        removed_in: "v0.12.1",
    },
    Removal {
        surface: DeprecatedSurface::Verb,
        old: "phux enroll",
        new: "phux host enroll",
        removed_in: "v0.12.1",
    },
    Removal {
        surface: DeprecatedSurface::Flag,
        old: "phux insert-pane --horizontal",
        new: "phux insert-pane --split horizontal",
        removed_in: "v0.12.1",
    },
    Removal {
        surface: DeprecatedSurface::Flag,
        old: "phux insert-pane --vertical",
        new: "phux insert-pane --split vertical",
        removed_in: "v0.12.1",
    },
    Removal {
        surface: DeprecatedSurface::Flag,
        old: "phux move-pane --horizontal",
        new: "phux move-pane --split horizontal",
        removed_in: "v0.12.1",
    },
    Removal {
        surface: DeprecatedSurface::Flag,
        old: "phux move-pane --vertical",
        new: "phux move-pane --split vertical",
        removed_in: "v0.12.1",
    },
];
