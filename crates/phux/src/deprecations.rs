//! The single deprecation table: one row per old spelling the binary still
//! accepts behind a hidden alias. `tests/configuration/deprecated_aliases.rs`
//! runs every row against the real binary, `refdocs::deprecations` renders
//! `docs/reference/deprecations.md` from it, and a clap-tree test in `lib.rs`
//! pins the table to the parser's hidden surface.
//!
//! Self-contained (no `crate::` paths or imports) because the audit test
//! compiles this file directly via `#[path]`.

/// Which kind of hidden surface carries an old spelling.
#[allow(
    dead_code,
    reason = "constructed by the next flag row added to DEPRECATED"
)]
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

/// Every deprecated spelling the binary currently accepts. `phux host
/// enroll` stays past its planned removal because Cockpit's add-machine flow
/// still invokes it (capability `host-enroll-v1`).
pub(crate) const DEPRECATED: &[Deprecation] = &[Deprecation {
    surface: DeprecatedSurface::Verb,
    old: "phux host enroll",
    new: "phux host add",
    note: "phux: `phux host enroll` is deprecated and will be removed; use `phux host add`",
    setup_argv: &[],
    // `--ssh-only` registers without contacting the host, so the row runs
    // to success with no ssh and no server.
    example_argv: &["host", "enroll", "me@mini", "--ssh-only"],
    deprecated_in: "v0.37.0",
    removed_in: "v0.39.0",
}];
