//! GitHub's enums, read leniently: an unknown value is `Unknown`, not an error.

/// A pull request's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrState {
    Open,
    Closed,
    Merged,
    Unknown,
}

/// A pull request's review decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewDecision {
    Approved,
    ChangesRequested,
    ReviewRequired,
    Unknown,
}

/// One check's state, folded from check runs and commit statuses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckState {
    Pending,
    Success,
    Failure,
    Neutral,
    Skipped,
    Cancelled,
    Unknown,
}

/// Whether GitHub can merge the head into the base.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mergeability {
    Mergeable,
    Conflicting,
    /// GitHub has not computed it yet.
    #[default]
    Unknown,
}

impl Mergeability {
    pub(crate) fn parse(raw: Option<&str>) -> Self {
        match raw {
            Some("MERGEABLE") => Self::Mergeable,
            Some("CONFLICTING") => Self::Conflicting,
            _ => Self::Unknown,
        }
    }
}

/// GitHub's `mergeStateStatus`: what stands between the pull request and a merge.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MergeState {
    /// The head is behind the base and the branch requires it be current.
    Behind,
    /// Required reviews or checks are missing.
    Blocked,
    Clean,
    /// Conflicts.
    Dirty,
    Draft,
    /// Mergeable with passing checks; pre-receive hooks may still refuse.
    HasHooks,
    /// Mergeable, but non-required checks fail.
    Unstable,
    #[default]
    Unknown,
}

impl MergeState {
    pub(crate) fn parse(raw: Option<&str>) -> Self {
        match raw {
            Some("BEHIND") => Self::Behind,
            Some("BLOCKED") => Self::Blocked,
            Some("CLEAN") => Self::Clean,
            Some("DIRTY") => Self::Dirty,
            Some("DRAFT") => Self::Draft,
            Some("HAS_HOOKS") => Self::HasHooks,
            Some("UNSTABLE") => Self::Unstable,
            _ => Self::Unknown,
        }
    }
}

/// How a pull request lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

impl MergeMethod {
    pub(crate) fn parse(raw: Option<&str>) -> Option<Self> {
        match raw {
            Some("MERGE") => Some(Self::Merge),
            Some("SQUASH") => Some(Self::Squash),
            Some("REBASE") => Some(Self::Rebase),
            _ => None,
        }
    }

    pub(crate) const fn wire(self) -> &'static str {
        match self {
            Self::Merge => "MERGE",
            Self::Squash => "SQUASH",
            Self::Rebase => "REBASE",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Merge => "Merge commit",
            Self::Squash => "Squash and merge",
            Self::Rebase => "Rebase and merge",
        }
    }
}

/// What the repository allows, and what the viewer last used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeMethods {
    pub allowed: Vec<MergeMethod>,
    pub preferred: MergeMethod,
    pub auto_merge: bool,
    /// GitHub deletes the head branch itself after a merge.
    pub deletes_branch: bool,
}

impl Default for MergeMethods {
    fn default() -> Self {
        Self {
            allowed: vec![MergeMethod::Merge],
            preferred: MergeMethod::Merge,
            auto_merge: false,
            deletes_branch: false,
        }
    }
}

/// Which side of a diff a review line is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LineSide {
    /// The base: removed and context lines, by their old number.
    Left,
    /// The head: added and context lines, by their new number.
    Right,
}

impl LineSide {
    pub(crate) fn parse(raw: Option<&str>) -> Self {
        if raw == Some("LEFT") {
            Self::Left
        } else {
            Self::Right
        }
    }

    pub(crate) const fn wire(self) -> &'static str {
        match self {
            Self::Left => "LEFT",
            Self::Right => "RIGHT",
        }
    }
}

/// The viewer's "Viewed" checkbox on one file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ViewedState {
    #[default]
    Unviewed,
    Viewed,
    /// Viewed, then the file changed.
    Dismissed,
}

impl ViewedState {
    pub(crate) fn parse(raw: Option<&str>) -> Self {
        match raw {
            Some("VIEWED") => Self::Viewed,
            Some("DISMISSED") => Self::Dismissed,
            _ => Self::Unviewed,
        }
    }
}

/// A commit's combined status (`StatusState`).
pub(crate) fn rollup_state(raw: &str) -> CheckState {
    match raw {
        "SUCCESS" => CheckState::Success,
        "PENDING" | "EXPECTED" => CheckState::Pending,
        "FAILURE" | "ERROR" => CheckState::Failure,
        _ => CheckState::Unknown,
    }
}

/// One check run's state from its `status` and `conclusion`.
pub(crate) fn check_run_state(status: Option<&str>, conclusion: Option<&str>) -> CheckState {
    if status.is_some_and(|status| status != "COMPLETED") {
        return CheckState::Pending;
    }
    match conclusion.unwrap_or_default() {
        "SUCCESS" => CheckState::Success,
        "FAILURE" | "TIMED_OUT" | "STARTUP_FAILURE" | "ACTION_REQUIRED" => CheckState::Failure,
        "NEUTRAL" => CheckState::Neutral,
        "SKIPPED" => CheckState::Skipped,
        "CANCELLED" => CheckState::Cancelled,
        _ => CheckState::Unknown,
    }
}
