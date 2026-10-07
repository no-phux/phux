//! Adversarial admission on a paired QUIC listener
//! (`docs/spec/workload-auth.md` §3, §9).
//!
//! ADR-0116 replaced the nonce/incarnation/channel-binding proof with mTLS, so
//! the replay attacks that proof defended against become questions about the
//! TLS handshake. These cases pin the answers on a real listener:
//!
//! - **Replay.** The certificate is public (the registry, `phux workload
//!   list --public-keys`, any earlier handshake). Presenting it without its
//!   private key cannot finish the handshake, so nothing is admitted.
//! - **Restart and cross-authority replay.** A certificate is accepted only
//!   when it chains to this server's current workload CA. One issued by an
//!   earlier authority (re-initialized after the old CA was removed) or by
//!   another server's CA is refused at TLS even when the registry holds its
//!   public key, so registry membership alone never admits.
//!
//! Each case carries a positive control on the same listener, so a refusal
//! cannot pass for a listener that admits nothing.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

use super::super::Incoming as _;
use super::super::tls::{QUIC_ALPN, ensure_self_signed};
use super::{QuicAdmission, QuicListener};
use crate::workload::{
    ClientMaterial, RegisteredCredential, ReloadingWorkloadRegistry, WorkloadPaths,
};

/// One workload authority (CA plus registry) in its own directory.
struct Authority {
    _dir: tempfile::TempDir,
    paths: WorkloadPaths,
}

impl Authority {
    fn init() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths = WorkloadPaths {
            ca_cert: dir.path().join("ca.pem"),
            ca_key: dir.path().join("ca.key"),
            registry: dir.path().join("workload-keys"),
        };
        crate::workload::init_authority(&paths.ca_cert, &paths.ca_key).unwrap();
        Self { _dir: dir, paths }
    }

    /// Enroll `key` through a CSR: the issued chain (leaf, then CA) written
    /// to `out`, and the registry record committed.
    fn enroll(&self, key: &rcgen::KeyPair, out: &Path) -> RegisteredCredential {
        let csr = rcgen::CertificateParams::new(vec!["client".to_owned()])
            .unwrap()
            .serialize_request(key)
            .unwrap();
        let material = ClientMaterial::from_pem(csr.pem().unwrap().as_bytes()).unwrap();
        let expires = chrono::Utc::now().timestamp() + 3600;
        let prepared =
            crate::workload::prepare_enrollment(&self.paths, &material, expires).unwrap();
        std::fs::write(out, prepared.issued_chain_pem().unwrap()).unwrap();
        prepared
            .commit(&self.paths.registry, vec!["*@global".to_owned()], expires)
            .unwrap()
    }

    /// A paired listener: no bearer preamble, a client certificate verified
    /// against this CA and mapped through this registry.
    fn listener(&self, server: &ServerCert) -> (QuicListener, SocketAddr) {
        let ca = crate::workload::authority_certificate(&self.paths.ca_cert).unwrap();
        let registry =
            Arc::new(ReloadingWorkloadRegistry::load(self.paths.registry.clone()).unwrap());
        let listener = QuicListener::with_admission(
            "127.0.0.1:0".parse().unwrap(),
            &server.cert,
            &server.key,
            QuicAdmission::Open,
            Some((&ca, registry)),
        )
        .unwrap();
        let addr = listener.local_addr().unwrap();
        (listener, addr)
    }
}

/// The listener's own (self-signed) server certificate.
struct ServerCert {
    _dir: tempfile::TempDir,
    cert: PathBuf,
    key: PathBuf,
}

fn server_cert() -> ServerCert {
    let dir = tempfile::tempdir().unwrap();
    let cert = dir.path().join("cert.pem");
    let key = dir.path().join("key.pem");
    ensure_self_signed(&cert, &key).unwrap();
    ServerCert {
        _dir: dir,
        cert,
        key,
    }
}

/// Presents a fixed chain signed by a fixed key, with no consistency check:
/// what an attacker holding a copied certificate would build.
#[derive(Debug)]
struct Presents(Arc<rustls::sign::CertifiedKey>);

impl rustls::client::ResolvesClientCert for Presents {
    fn resolve(
        &self,
        _root_hint_subjects: &[&[u8]],
        _sigschemes: &[rustls::SignatureScheme],
    ) -> Option<Arc<rustls::sign::CertifiedKey>> {
        Some(Arc::clone(&self.0))
    }

    fn has_certs(&self) -> bool {
        true
    }
}

/// A QUIC client presenting `chain` and signing its handshake with `key`.
fn client(chain: &Path, key: &rcgen::KeyPair) -> quinn::Endpoint {
    let certs = CertificateDer::pem_file_iter(chain)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let key_der = PrivateKeyDer::from_pem_slice(key.serialize_pem().as_bytes()).unwrap();
    let signer = rustls::crypto::ring::sign::any_supported_type(&key_der).unwrap();
    let mut crypto = phux_dial::tls::client_config_with_identity(
        &phux_dial::CertTrust::SkipVerify,
        &phux_dial::TlsClientIdentity::None,
        Some(QUIC_ALPN),
    )
    .unwrap();
    crypto.client_auth_cert_resolver = Arc::new(Presents(Arc::new(
        rustls::sign::CertifiedKey::new(certs, signer),
    )));
    let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(crypto).unwrap(),
    )));
    endpoint
}

/// Dial and open the control stream, then report the credential the
/// listener stamped, or `None` when the listener closed the connection (or
/// the handshake failed) without admitting it.
async fn attempt(
    listener: &QuicListener,
    addr: SocketAddr,
    endpoint: &quinn::Endpoint,
) -> Option<crate::auth::AuthenticatedCredential> {
    let refused = async {
        let Ok(connecting) = endpoint.connect(addr, "localhost") else {
            return;
        };
        let Ok(conn) = connecting.await else {
            return;
        };
        if let Ok((mut send, _recv)) = conn.open_bi().await {
            // The stream exists for the listener only once it carries data.
            let _ = send.write_all(b"x").await;
        }
        conn.closed().await;
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::select! {
            accepted = listener.accept() => {
                let (_reader, _writer, peer) = accepted.expect("an admitted peer");
                Some(peer.credential.expect("a workload credential"))
            }
            () = refused => None,
        }
    })
    .await
    .expect("the attempt settles")
}

#[tokio::test]
async fn replay_of_an_enrolled_certificate_without_its_private_key_is_refused() {
    let server = server_cert();
    let authority = Authority::init();
    let dir = tempfile::tempdir().unwrap();
    let victim = rcgen::KeyPair::generate().unwrap();
    let chain = dir.path().join("victim.pem");
    let enrolled = authority.enroll(&victim, &chain);
    let (listener, addr) = authority.listener(&server);

    let thief = rcgen::KeyPair::generate().unwrap();
    assert_eq!(
        attempt(&listener, addr, &client(&chain, &thief)).await,
        None,
        "the victim's certificate signed with another key is refused"
    );

    let admitted = attempt(&listener, addr, &client(&chain, &victim))
        .await
        .expect("control: the holder of the key is admitted");
    assert_eq!(admitted.id, enrolled.id);
}

#[tokio::test]
async fn a_certificate_from_an_earlier_or_foreign_authority_is_refused_though_its_key_is_enrolled()
{
    let server = server_cert();
    let dir = tempfile::tempdir().unwrap();
    let key = rcgen::KeyPair::generate().unwrap();

    // The certificate an earlier incarnation of the authority (or another
    // server) issued for this key.
    let earlier = Authority::init();
    let earlier_chain = dir.path().join("earlier.pem");
    let earlier_credential = earlier.enroll(&key, &earlier_chain);

    // The current authority enrolls the same key, so its registry holds the
    // very credential id the earlier certificate maps to.
    let current = Authority::init();
    let current_chain = dir.path().join("current.pem");
    let current_credential = current.enroll(&key, &current_chain);
    assert_eq!(
        earlier_credential.id, current_credential.id,
        "the credential id is the key's, whichever CA issued the certificate"
    );
    let (listener, addr) = current.listener(&server);

    assert_eq!(
        attempt(&listener, addr, &client(&earlier_chain, &key)).await,
        None,
        "a chain to another CA is refused although the registry knows the key"
    );

    let admitted = attempt(&listener, addr, &client(&current_chain, &key))
        .await
        .expect("control: the current authority's certificate is admitted");
    assert_eq!(admitted.id, current_credential.id);
}
