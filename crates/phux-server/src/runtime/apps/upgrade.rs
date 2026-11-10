//! Carrying every kind across a graceful upgrade (ADR-0032) through its app.
//!
//! At the cut each app chooses its resources, cuts them from their engines,
//! and writes its part of the blob; on resume each rebuilds that part. The
//! host iterates [`APPS`] and names no kind. The blob's fields are a frozen
//! format, so an app writes only the fields its kind already owns.

use std::os::fd::RawFd;
use std::time::Duration;

use futures_util::future::LocalBoxFuture;

use super::APPS;
use crate::runtime::connections::pane_events::PaneEvents;
use crate::runtime::upgrade::UpgradeError;
use crate::state::upgrade_blob::{KindRebuild, RebuildError, RebuiltTree};
use crate::state::{ServerState, SharedState};
use crate::upgrade::blob::StateBlob;

/// One kind's part of one graceful upgrade, from the cut to the exec. The
/// host holds it until exec; dropping it releases whatever it still holds,
/// and a successful exec replaces the process instead.
pub(crate) trait UpgradeCut {
    /// Choose the resources to carry and return the future that cuts them.
    /// The choice happens in this call, not when the future first runs, so
    /// the caller makes it in the same step as its tree identity. A
    /// resource the kind cannot strand fails the upgrade; one it can is
    /// left behind.
    fn cut<'a>(
        &'a mut self,
        state: &SharedState,
        deadline: Duration,
    ) -> LocalBoxFuture<'a, Result<(), UpgradeError>>;

    /// Write what was cut into `blob`, under the lock that found the tree
    /// unchanged. Fails when a resource this kind may not drop has no cut.
    fn carry(&self, state: &ServerState, blob: &mut StateBlob) -> Result<(), UpgradeError>;

    /// The descriptors in `blob` this kind's resources need across exec.
    fn inherited_fds(&self, blob: &StateBlob) -> Vec<RawFd>;

    /// The upgrade did not happen: release what the cut holds within a
    /// bounded window. Whatever misses it is left to drop.
    fn lift(&mut self) -> LocalBoxFuture<'_, ()>;
}

/// One kind's half of a resume, beyond rebuilding its engines.
pub(crate) trait KindResume: KindRebuild {
    /// The rebuilt tree is committed and the lock released: start this
    /// kind's runtime wiring, handing back the events each rebuilt
    /// resource's exit watcher drains.
    fn start(&mut self, state: &SharedState) -> Vec<(phux_core::ids::ResourceId, PaneEvents)>;
}

/// Begin carrying every kind, or `None` while another upgrade holds one.
pub(crate) fn begin(state: &SharedState) -> Option<Vec<Box<dyn UpgradeCut>>> {
    APPS.iter().map(|app| app.begin_upgrade(state)).collect()
}

/// Every kind's half of a resume, made before the rebuild takes the lock.
pub(crate) fn resumes(state: &SharedState) -> Vec<Box<dyn KindResume>> {
    APPS.iter().map(|app| app.resume(state)).collect()
}

/// Rebuild `blob` into `state` through `resumes`, in [`APPS`] order: the
/// Terminal, every other kind's parent, comes first.
///
/// # Errors
///
/// See [`ServerState::rebuild_from_blob`].
pub(crate) fn rebuild(
    state: &mut ServerState,
    blob: &StateBlob,
    resumes: &mut [Box<dyn KindResume>],
) -> Result<RebuiltTree, RebuildError> {
    let mut kinds: Vec<&mut dyn KindRebuild> = resumes
        .iter_mut()
        .map(|resume| &mut **resume as &mut dyn KindRebuild)
        .collect();
    state.rebuild_from_blob(blob, &mut kinds)
}
