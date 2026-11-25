//! What an entry point hands the loop after building it.

use super::Jobs;

impl super::SessionLoop {
    pub(in crate::attach::driver) fn set_control_dial(&mut self, dial: crate::attach::Dial) {
        self.control_dial = Some(Box::new(dial));
    }

    /// Run background jobs on `jobs` instead of this loop's own facility.
    pub(in crate::attach::driver) fn set_jobs(&mut self, jobs: Jobs) {
        self.jobs = jobs;
    }
}
