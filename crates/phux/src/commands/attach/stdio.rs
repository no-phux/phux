//! Attach through `ssh HOST phux stdio-bridge`: the local client on a host
//! UDP cannot reach, with the user's ssh config, jump hosts, and keys.

use std::process::ExitCode;
use std::time::Duration;

use phux_client::attach::{AttachError, Dial};

use super::{
    attach_with_reconnect, fallback_lookup_name, finalize_recording, predictive_config_for,
    report_attach_end, requested_attach_target, resolved_default_session_name,
};
use crate::commands::rec::RecordSpec;

/// Run the attach loop over an ssh stdio bridge to `destination`.
///
/// # Errors
///
/// Why it never attached, unreported, so the caller can try another way.
pub(crate) fn run_attach_stdio(
    destination: &str,
    remote_phux: &str,
    session: Option<String>,
    rec: Option<&RecordSpec>,
) -> Result<ExitCode, AttachError> {
    let rt = match crate::commands::cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return Ok(code),
    };
    let dial = Dial::Ssh {
        destination: destination.to_owned(),
        remote_phux: remote_phux.to_owned(),
        askpass: None,
    };
    let default_name = resolved_default_session_name();
    let default = session
        .is_none()
        .then(|| fallback_lookup_name(&default_name))
        .flatten();
    let result = rt.block_on(attach_with_reconnect(
        &dial,
        requested_attach_target(session),
        predictive_config_for(&dial),
        default,
        rec,
    ));
    finalize_recording(rec);
    // An idle stdin read cannot be cancelled; do not wait on it.
    rt.shutdown_timeout(Duration::ZERO);
    match result {
        Ok(end) => {
            report_attach_end(end);
            Ok(ExitCode::SUCCESS)
        }
        // The reconnect window already reported this.
        Err(AttachError::Disconnected) => Ok(ExitCode::FAILURE),
        Err(err) => Err(err),
    }
}
