//! The QUIC probe of a bootstrapped listener, whose connection the attach
//! then starts from.

use phux_client::attach::connection::Connection;

use super::Report;

/// Connect to the bootstrapped listener within the probe deadline, which
/// only a filtered path waits out, negotiating as the attach does.
pub(super) fn probe(
    rt: &tokio::runtime::Runtime,
    target: &str,
    report: &Report,
    identity: Option<phux_dial::TlsClientIdentity>,
) -> Result<(super::attach::DialPlan, Box<Connection>), String> {
    let plan = super::attach::plan_quic_dial(
        rt,
        target,
        Some(report.token.clone()),
        Some(report.cert_fingerprint.clone()),
        None,
    )
    .and_then(|plan| {
        // The leaf pin arrived over ssh this second; nothing to learn.
        plan.with_identity(identity).with_authority(
            target,
            &phux_client_runtime::target::AuthorityPin::default(),
        )
    })
    .map_err(|refusal| format!("{refusal:?}"))?;
    let deadline = super::super::enroll::probe_deadline();
    let connected = rt.block_on(async {
        tokio::time::timeout(deadline, phux_tui::attach::connect_for_attach(&plan.dial)).await
    });
    // Noted like every probe, for [`super::super::enroll::udp_unanswered`].
    match connected {
        Ok(Ok(connection)) => {
            super::super::enroll::record_udp_probe(target, true);
            Ok((plan, Box::new(connection)))
        }
        Ok(Err(err)) => Err(err.to_string()),
        Err(_) => {
            super::super::enroll::record_udp_probe(target, false);
            Err(format!(
                "no answer within {}s; is UDP filtered?",
                deadline.as_secs_f64()
            ))
        }
    }
}
