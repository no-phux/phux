//! Thin headless upload consumer. File retention and replay belong to the managed helper.
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use phux_client::attach::{CertTrust, Dial};
use phux_client::selector;
use phux_client::upload::{UploadOutcome, UploadReceipt};
use phux_client_runtime::runtime::Target;
use phux_client_runtime::target::AuthorityPin;

use super::json_err::{self, CliError, codes};
use super::server_target::ServerSpec;

pub(crate) fn run(
    raw: &str,
    file: PathBuf,
    extension: Option<String>,
    timeout: u64,
    json: bool,
    spec: ServerSpec,
) -> ExitCode {
    let start = Instant::now();
    if file.as_os_str().is_empty()
        || !(1..=3600).contains(&timeout)
        || extension.as_deref().is_some_and(|ext| {
            ext.is_empty()
                || ext.len() > 16
                || !ext.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
    {
        return fail(
            json,
            "timeout must be 1..=3600 seconds and extension must be 1–16 ASCII alphanumeric characters",
            2,
        );
    }
    let selector = match selector::parse(raw) {
        Ok(value) => value,
        Err(err) => {
            return json_err::emit(
                json,
                &CliError::new(
                    codes::INVALID_SELECTOR,
                    err.to_string(),
                    "use a headless target selector",
                ),
                2,
            );
        }
    };
    // Remote planning can block in DNS. It never uploads and upload resolution never pairs.
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    if let Err(err) = std::thread::Builder::new()
        .name("upload-resolve".into())
        .spawn(move || {
            let _ = sender.send(spec.prepare("upload", json));
        })
    {
        return fail(
            json,
            &format!("could not start target resolution: {err}"),
            1,
        );
    }
    let (rt, server) =
        match receiver.recv_timeout(Duration::from_secs(timeout).saturating_sub(start.elapsed())) {
            Ok(Ok(value)) => value,
            Ok(Err(code)) => return code,
            Err(_) => {
                return fail(
                    json,
                    "upload deadline elapsed during server resolution; no transfer started",
                    1,
                );
            }
        };
    let Some(budget) = Duration::from_secs(timeout).checked_sub(start.elapsed()) else {
        return fail(json, "upload deadline elapsed before target resolution", 1);
    };
    let status = rt.block_on(async {
        let deadline = tokio::time::Instant::now() + budget;
        let resolved =
            tokio::time::timeout_at(deadline, resolve_terminal(&server, &selector, raw, json))
                .await;
        let terminal = match resolved {
            Ok(Ok(value)) => value,
            Ok(Err(code)) => return code,
            Err(_) => {
                return fail(
                    json,
                    "upload deadline elapsed during target resolution; no transfer started",
                    1,
                );
            }
        };
        // Do not timeout/cancel the helper: it must close its runtime and return an unknown receipt.
        match phux_client::upload::upload_file(
            managed_target(server.dial()),
            terminal,
            file,
            extension,
            deadline.saturating_duration_since(tokio::time::Instant::now()),
        )
        .await
        {
            Ok(receipt) => print_receipt(&receipt, json),
            Err(err) => fail(json, &err.to_string(), 1),
        }
    });
    // A cancelled local reader may still be running; it cannot connect or upload.
    rt.shutdown_background();
    status
}

async fn resolve_terminal(
    server: &super::server_target::ServerTarget,
    selector: &selector::Selector,
    raw: &str,
    json: bool,
) -> Result<phux_protocol::ResourceId, ExitCode> {
    let mut conn = server
        .connect()
        .await
        .map_err(|err| server.report_unreachable(json, &err, "upload"))?;
    let view = phux_client::state::get_state_on(&mut conn)
        .await
        .map_err(|err| server.report_unreachable(json, &err, "upload"))?;
    let candidates = phux_client::state::resolve_targets_on(&mut conn, selector, view.snapshot())
        .await
        .map_err(|err| super::report_agent_resolve_error(json, &err, true))?;
    drop(conn);
    selector::pick_target_pane(&candidates, &view.snapshot().focused_resource).ok_or_else(|| {
        super::partial::report_target_miss_keeping_status_for(json, Some(raw), view.degradation())
    })
}

fn print_receipt(receipt: &UploadReceipt, json: bool) -> ExitCode {
    let completed = receipt.outcome == UploadOutcome::Completed;
    if json {
        outln!(
            "{}",
            serde_json::json!({"schema_version": 1, "receipt": receipt})
        );
    } else if let Some(path) = &receipt.path {
        outln!("{path}");
    }
    if completed {
        ExitCode::SUCCESS
    } else {
        eprintln!(
            "phux: upload {:?}: {}; do not blindly retry",
            receipt.outcome, receipt.message
        );
        ExitCode::FAILURE
    }
}

fn fail(json: bool, message: &str, status: u8) -> ExitCode {
    json_err::emit(
        json,
        &CliError::new(
            codes::TRANSPORT,
            message,
            "check the local file and target; do not blindly retry a started transfer",
        ),
        status,
    )
}

/// Adapt the already validated headless dial without resolving/pairing a second time.
fn managed_target(dial: Dial) -> Target {
    let (mut target, trust) = match dial {
        Dial::Uds(path) => return Target::uds(path),
        Dial::Ws(ws) => {
            let mut target = Target::ws(ws.url);
            target.token = ws.token;
            target.tls_server_name = ws.tls_server_name;
            target.client_identity = ws.identity.unwrap_or(phux_dial::TlsClientIdentity::None);
            (target, ws.trust)
        }
        Dial::Quic(quic) => {
            let mut target = Target::quic(quic.addr.to_string());
            target.token = quic.token.map(|bytes| {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                bytes
                    .into_iter()
                    .flat_map(|byte| {
                        [
                            char::from(HEX[usize::from(byte >> 4)]),
                            char::from(HEX[usize::from(byte & 15)]),
                        ]
                    })
                    .collect()
            });
            target.tls_server_name = Some(quic.server_name);
            target.client_identity = quic.identity.unwrap_or(phux_dial::TlsClientIdentity::None);
            if let Some(inner) = quic.inner {
                target.authority.route = Some(inner.server_name);
                target.client_identity = inner.identity;
                if let CertTrust::Authority { ca, .. } = inner.trust {
                    target.authority.ca = Some(ca);
                }
            }
            (target, quic.trust)
        }
    };
    match trust {
        CertTrust::SkipVerify => {}
        CertTrust::Pinned(leaf) => target.cert_fingerprint = Some(leaf),
        CertTrust::PinnedLearning { leaf, learn } => {
            target.cert_fingerprint = Some(leaf);
            target.authority.learner = Some(learn);
        }
        CertTrust::Authority { ca, leaf } => {
            target.cert_fingerprint = leaf;
            target.authority = AuthorityPin {
                ca: Some(ca),
                ..target.authority
            };
        }
    }
    target
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]
    use super::*;
    use phux_client::attach::{QuicDial, WsDial};
    use phux_client_runtime::runtime::Transport;
    use phux_dial::TlsClientIdentity;

    #[test]
    fn managed_target_preserves_local_path_and_remote_credentials() {
        assert_eq!(
            managed_target(Dial::uds(std::path::Path::new("/tmp/private.sock"))),
            Target::uds("/tmp/private.sock")
        );
        let learner = phux_dial::AuthorityLearner::new(|_| {});
        for trust in [
            CertTrust::SkipVerify,
            CertTrust::Pinned("leaf".into()),
            CertTrust::PinnedLearning {
                leaf: "leaf".into(),
                learn: learner.clone(),
            },
            CertTrust::Authority {
                ca: "ca".into(),
                leaf: Some("leaf".into()),
            },
        ] {
            let target = managed_target(Dial::Ws(WsDial {
                url: "wss://host:8787".into(),
                token: Some("abcd".into()),
                trust: trust.clone(),
                tls_server_name: Some("sni".into()),
                identity: Some(TlsClientIdentity::None),
            }));
            assert_eq!(target.transport, Transport::Ws("wss://host:8787".into()));
            assert_eq!(target.token.as_deref(), Some("abcd"));
            assert_eq!(target.tls_server_name.as_deref(), Some("sni"));
            match trust {
                CertTrust::SkipVerify => assert!(target.cert_fingerprint.is_none()),
                CertTrust::Pinned(_) => {
                    assert_eq!(target.cert_fingerprint.as_deref(), Some("leaf"));
                }
                CertTrust::PinnedLearning { .. } => {
                    assert_eq!(target.authority.learner, Some(learner.clone()));
                }
                CertTrust::Authority { .. } => {
                    assert_eq!(target.authority.ca.as_deref(), Some("ca"));
                }
            }
        }
        let target = managed_target(Dial::Quic(QuicDial {
            addr: "127.0.0.1:8788".parse().unwrap(),
            server_name: "relay".into(),
            token: Some(vec![0, 15, 255]),
            trust: CertTrust::Pinned("relay-leaf".into()),
            identity: Some(TlsClientIdentity::None),
            inner: Some(phux_dial::quic::InnerTls {
                trust: CertTrust::Authority {
                    ca: "server-ca".into(),
                    leaf: None,
                },
                identity: TlsClientIdentity::None,
                server_name: "server-route".into(),
            }),
        }));
        assert_eq!(target.transport, Transport::Quic("127.0.0.1:8788".into()));
        assert_eq!(target.token.as_deref(), Some("000fff"));
        assert_eq!(target.cert_fingerprint.as_deref(), Some("relay-leaf"));
        assert_eq!(target.authority.ca.as_deref(), Some("server-ca"));
        assert_eq!(target.authority.route.as_deref(), Some("server-route"));
        assert_eq!(target.tls_server_name.as_deref(), Some("relay"));
    }
}
