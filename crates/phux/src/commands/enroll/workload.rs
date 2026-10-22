//! Enrolling and settling a workload client certificate: minting a ticket
//! over ssh, redeeming it, storing the pair, and retiring the old one.

use phux_dial::TlsClientIdentity;
use phux_server::workload::ClientRequest;

use super::events::{EnrollEvent, Retired};
use super::flow::{FarHost, HOST_ADD_WORKLOAD_SCOPE};
use super::identity::{
    CERT_EXTENSION, ClientIdentityFiles, HeldIdentity, KEY_EXTENSION, WorkloadEnrollment,
};
use super::pair::{json_document_in, probe_deadline};
use super::ssh::{SSH_FAILED, ssh_run, ssh_run_with_stdin};

/// `phux workload revoke`'s refusal for an id its registry does not hold.
pub(super) const NO_SUCH_CREDENTIAL: &str = "no workload credential";

/// What [`settle_identity`] did with the credential the new pair replaced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Settlement {
    /// The far-host id of the credential the entry named before, when it
    /// could be read and differs from the new one.
    pub(crate) previous_credential_id: Option<String>,
    /// Whether that credential is revoked now; `None` when there was none
    /// to revoke, it could not be named, or this host does not hold it.
    pub(crate) previous_revoked: Option<bool>,
}

/// The second phase of a (re-)enrollment, run after the registry write.
///
/// When the entry now names `fresh` (`recorded`), the credential `held`
/// named is revoked on the far host and the files an enrollment wrote for
/// it are removed. When it does not, `fresh` is the orphan: its files are
/// removed and its credential revoked, and the entry keeps what it held.
/// Revoking only after the entry moved means no failure leaves the entry
/// on a credential the far host already refused; the cost is a window,
/// between the two ssh calls, in which both credentials of this one client
/// are admitted.
pub(crate) fn settle_identity(
    far: &FarHost<'_>,
    fresh: Option<&ClientIdentityFiles>,
    recorded: bool,
    held: &HeldIdentity,
    workload: &WorkloadEnrollment<'_>,
    on_event: &mut dyn FnMut(EnrollEvent),
) -> Settlement {
    let Some(fresh) = fresh.filter(|identity| identity.fresh) else {
        return Settlement::default();
    };
    if !recorded {
        fresh.discard_if_fresh();
        if let Some(id) = &fresh.credential_id {
            far.retire(id, Retired::Unrecorded, on_event);
        }
        return Settlement::default();
    }
    let mut settlement = Settlement::default();
    match held.credential_id(workload.dir, workload.name) {
        Some(previous) if fresh.credential_id.as_deref() != Some(previous.as_str()) => {
            settlement.previous_revoked = far.retire(&previous, Retired::Superseded, on_event);
            settlement.previous_credential_id = Some(previous);
        }
        Some(_) => {}
        None if held.is_empty() => {}
        None => on_event(EnrollEvent::PreviousCertificateUnidentified {
            remedy: format!("ssh {} {} workload list", far.ssh_host, far.remote_phux),
        }),
    }
    for path in held.enrolled_files(workload.dir, workload.name) {
        if path != fresh.certificate && path != fresh.private_key {
            phux_server::workload::remove_owned_file(&path);
        }
    }
    settlement
}

/// Enroll a workload client certificate on the far host (ADR-0116):
/// generate the key here, send only its CSR over ssh stdin to `phux
/// workload add-key --cert-stdout`, validate the returned chain against the
/// key, and store both owner-only under new names. Nothing is revoked here:
/// the credential this one supersedes goes in [`settle_identity`], once the
/// entry names the new pair. If the far host enrolled the key but a check
/// or the store fails here, that new credential (whose key is then gone) is
/// revoked before the error returns.
pub(crate) fn enroll_client_certificate(
    far: &FarHost<'_>,
    workload: &WorkloadEnrollment<'_>,
    on_event: &mut dyn FnMut(EnrollEvent),
) -> Result<ClientIdentityFiles, String> {
    let (ssh_host, remote_phux) = (far.ssh_host, far.remote_phux);
    // The name becomes a file name: refuse one the registry would refuse
    // before anything is written here or changed on the far host.
    if super::super::remote::validate_name(workload.name).as_deref() != Ok(workload.name) {
        return Err(format!("{:?} is not a valid remote name", workload.name));
    }
    let authority = ssh_run(
        ssh_host,
        &[remote_phux, "workload", "authority", "--init", "--json"],
    )?;
    if !authority.status.success() {
        return Err(format!(
            "the host could not initialize its workload authority: {}",
            authority.failure_detail()
        ));
    }
    let request = ClientRequest::generate()
        .map_err(|err| format!("could not generate a client key: {err}"))?;
    let argv = [
        remote_phux,
        "workload",
        "add-key",
        "--json",
        "--cert-stdout",
        "--scope",
        HOST_ADD_WORKLOAD_SCOPE,
    ];
    let added = ssh_run_with_stdin(ssh_host, &argv, request.csr_pem().as_bytes())?;
    if !added.status.success() {
        // ssh itself failed: the far host may have committed before the
        // channel dropped, so retire this request's own id (a host that
        // never enrolled it answers "not held", which says nothing).
        if added.status.code() == Some(SSH_FAILED) {
            far.retire(&request.credential_id(), Retired::Unrecorded, on_event);
        }
        return Err(format!(
            "`phux workload add-key` failed there: {}",
            added.failure_detail()
        ));
    }
    accept_and_store(&request, &added.stdout, workload).inspect_err(|_| {
        // The far host may hold a credential for a key that is gone now.
        // Only this request's own id is revoked, never one the untrusted
        // reply named.
        far.retire(&request.credential_id(), Retired::Unrecorded, on_event);
    })
}

/// The local half of [`enroll_client_certificate`]: check the reply, bind it
/// to the key, and store both owner-only.
pub(super) fn accept_and_store(
    request: &ClientRequest,
    stdout: &str,
    workload: &WorkloadEnrollment<'_>,
) -> Result<ClientIdentityFiles, String> {
    let reply = AddKeyReply::parse(stdout)?;
    if reply.credential_id != request.credential_id() {
        return Err("the host recorded a different credential than the key sent".to_owned());
    }
    let issued = request
        .accept(&reply.certificate_chain)
        .map_err(|err| err.to_string())?;
    let stem = identity_stem(workload.name, issued.credential_id());
    let files = ClientIdentityFiles {
        certificate: workload.dir.join(format!("{stem}{CERT_EXTENSION}")),
        private_key: workload.dir.join(format!("{stem}{KEY_EXTENSION}")),
        credential_id: Some(issued.credential_id().to_owned()),
        fresh: true,
    };
    issued
        .store(&files.private_key, &files.certificate)
        .map_err(|err| format!("could not store the client identity: {err}"))?;
    Ok(files)
}

/// Enroll a fresh local key with the single-use `ticket_hex` a connect link
/// carried (ADR-0154): over the link's `quic` endpoint, under the enrollment
/// ALPN, trusting the server by the link's pins. The link must pin the
/// server's CA (`ca`), which is never learned from the reply (ADR-0179). The
/// reply is checked as an ssh enrollment's is ([`ClientRequest::accept`]),
/// its CA must be the link's `ca`, and the pair is stored owner-only in
/// `workload.dir`.
///
/// # Errors
///
/// Wording for the operator; never the ticket or the reply.
pub(crate) fn enroll_with_ticket(
    quic_target: &str,
    pins: &TicketPins<'_>,
    ticket_hex: &str,
    workload: &WorkloadEnrollment<'_>,
) -> Result<ClientIdentityFiles, String> {
    let ticket = phux_dial::quic::parse_token_hex(ticket_hex)
        .ok()
        .filter(|ticket| ticket.len() == phux_server::workload::tickets::TICKET_BYTES)
        .ok_or_else(|| "the link's enrollment ticket is not 64 hex digits".to_owned())?;
    let Some(pinned) = pins.authority else {
        return Err(
            "an enrollment link must pin the server's `ca`; mint a new one with `phux pair --enroll`"
                .to_owned(),
        );
    };
    let request = ClientRequest::generate()
        .map_err(|err| format!("could not generate a client key: {err}"))?;
    let csr = match phux_server::workload::ClientMaterial::from_pem(request.csr_pem().as_bytes()) {
        Ok(phux_server::workload::ClientMaterial::Request(der)) => der.as_ref().to_vec(),
        _ => return Err("could not encode the enrollment request".to_owned()),
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| format!("could not build a runtime: {err}"))?;
    let pin = phux_client_runtime::target::AuthorityPin {
        ca: Some(pinned.to_owned()),
        route: pins.route.map(str::to_owned),
    };
    let plan = super::super::attach::plan_quic_dial(
        &rt,
        quic_target,
        None,
        pins.leaf.map(str::to_owned),
        pins.route.map(str::to_owned),
    )
    .and_then(|plan| plan.with_authority(quic_target, &pin))
    .map_err(|refusal| format!("{refusal:?}"))?;
    let phux_client::attach::Dial::Quic(dial) = plan.dial else {
        return Err("enrollment needs a quic:// endpoint".to_owned());
    };
    let enroll_dial = phux_dial::enroll::EnrollDial {
        addr: dial.addr,
        server_name: dial.server_name,
        trust: dial.trust,
        inner: pin.inner(&TlsClientIdentity::None),
    };
    let chain = rt
        .block_on(async {
            tokio::time::timeout(
                probe_deadline(),
                phux_dial::enroll::enroll(
                    &enroll_dial,
                    &phux_protocol::enroll::Request { ticket, csr },
                ),
            )
            .await
        })
        .map_err(|_| "the server did not answer the enrollment in time".to_owned())?
        .map_err(|err| err.to_string())?;
    let issued = request.accept(&chain).map_err(|err| err.to_string())?;
    if phux_config::known_authorities::canonical_authority(pinned).as_deref()
        != Some(issued.ca_fingerprint())
    {
        return Err("the issuing CA is not the one the link pins".to_owned());
    }
    let stem = identity_stem(workload.name, issued.credential_id());
    let files = ClientIdentityFiles {
        certificate: workload.dir.join(format!("{stem}{CERT_EXTENSION}")),
        private_key: workload.dir.join(format!("{stem}{KEY_EXTENSION}")),
        credential_id: Some(issued.credential_id().to_owned()),
        fresh: true,
    };
    issued
        .store(&files.private_key, &files.certificate)
        .map_err(|err| format!("could not store the client identity: {err}"))?;
    Ok(files)
}

/// The pins a connect link gave for a ticket enrollment.
pub(crate) struct TicketPins<'a> {
    /// The leaf fingerprint (`fp`): the relay's on a relay link.
    pub(crate) leaf: Option<&'a str>,
    /// The CA fingerprint (`ca`): the server's.
    pub(crate) authority: Option<&'a str>,
    /// The relay route (`sni`), when the link reaches the server through a
    /// relay (ADR-0154 item 5).
    pub(crate) route: Option<&'a str>,
}

/// `<name>.client.<first 16 hex digits of the credential id>`: one pair of
/// files per enrollment, so a re-enrollment never overwrites the pair the
/// registry still names.
pub(super) fn identity_stem(name: &str, credential_id: &str) -> String {
    let digest = credential_id
        .strip_prefix("sha256:")
        .unwrap_or(credential_id);
    let short = digest.get(..16).unwrap_or(digest);
    format!("{name}.client.{short}")
}

/// What `phux workload add-key --json --cert-stdout` reported on the far
/// host. Untrusted: every field is checked here or by
/// [`ClientRequest::accept`].
#[derive(Debug)]
pub(super) struct AddKeyReply {
    pub(super) credential_id: String,
    pub(super) certificate_chain: String,
}

impl AddKeyReply {
    pub(super) fn parse(stdout: &str) -> Result<Self, String> {
        let is_document = |value: &serde_json::Value| {
            value.get("operation").and_then(serde_json::Value::as_str) == Some("add-key")
        };
        let document = json_document_in(stdout, is_document)
            .ok_or_else(|| "`phux workload add-key` reported no document".to_owned())?;
        let field = |name: &str| {
            document
                .get(name)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| {
                    format!(
                        "`phux workload add-key` reported no {name}; is phux on the host older than this one?"
                    )
                })
        };
        let credential_id = field("credential_id")?;
        if !phux_server::workload::is_canonical_credential_id(&credential_id) {
            return Err("the host reported a malformed credential id".to_owned());
        }
        Ok(Self {
            credential_id,
            certificate_chain: field("certificate_chain")?,
        })
    }
}
