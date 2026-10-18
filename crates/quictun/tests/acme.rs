//! ACME through the relay, end to end, against an in-process CA, then
//! multiplexed `connect` over the certificate it issued.
//!
//! The mock CA speaks the RFC 8555 subset the client uses. It verifies every
//! JWS, uses each nonce once, refuses the first nonce with `badNonce` to
//! exercise the retry, validates TLS-ALPN-01 the way a real CA does (dialing
//! the relay's TCP ingress with the `acme-tls/1` ALPN and checking the
//! critical acmeIdentifier extension), and signs the client's CSR.

#![cfg(all(feature = "acme", feature = "relay"))]
#![allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use quictun::Protocol;
use quictun::acme::{AcmeCerts, AcmeConfig, issue, service_config};
use quictun::connect::{RelayAddr, Tunnel};
use quictun::forward::{self, Route, ServiceTls, ServiceTrust, serve_tunnel};
use quictun::relay::{RelayConfig, RelayRuntime, TcpIngress, cert_fingerprint, mint_route_token};
use ring::signature::{ECDSA_P256_SHA256_FIXED, UnparsedPublicKey};
use serde_json::{Value, json};
use sha2::Digest as _;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::time::timeout;

const DEADLINE: Duration = Duration::from_secs(20);
const DOMAIN: &str = "laptop.relay.test";

/// The test CA: its root, and an issuer for the CA's HTTPS and for CSRs.
struct TestCa {
    root_pem: String,
    issuer: rcgen::Issuer<'static, rcgen::KeyPair>,
}

impl TestCa {
    fn new() -> Self {
        let mut params = rcgen::CertificateParams::new(Vec::new()).unwrap();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "quictun test CA");
        let key = rcgen::KeyPair::generate().unwrap();
        let root = params.self_signed(&key).unwrap();
        Self {
            root_pem: root.pem(),
            issuer: rcgen::Issuer::new(params, key),
        }
    }

    fn roots(&self) -> rustls::RootCertStore {
        let mut roots = rustls::RootCertStore::empty();
        for cert in rustls::pki_types::CertificateDer::pem_slice_iter(self.root_pem.as_bytes()) {
            roots.add(cert.unwrap()).unwrap();
        }
        roots
    }

    /// A leaf for `name`, its key PEM, signed by this CA.
    fn leaf(&self, name: &str) -> (String, String) {
        let key = rcgen::KeyPair::generate().unwrap();
        let params = rcgen::CertificateParams::new(vec![name.to_owned()]).unwrap();
        let cert = params.signed_by(&key, &self.issuer).unwrap();
        (cert.pem(), key.serialize_pem())
    }
}

use rustls::pki_types::pem::PemObject as _;

/// The mock CA's state across requests.
#[derive(Default)]
struct CaState {
    base: String,
    nonces: HashSet<String>,
    next_nonce: u64,
    refused_a_nonce: bool,
    account_key: Option<Vec<u8>>,
    token: String,
    authz_status: &'static str,
    order_status: &'static str,
    chain: Option<String>,
}

struct MockCa {
    ca: TestCa,
    state: Mutex<CaState>,
    /// Where validation dials: the relay's TCP ingress.
    ingress: SocketAddr,
}

/// One parsed HTTP request.
struct Request {
    method: String,
    path: String,
    /// The `Host` header, from which (like Pebble and Boulder) the CA
    /// builds the URLs it hands out.
    host: String,
    body: Vec<u8>,
}

async fn read_request<S: tokio::io::AsyncRead + Unpin>(stream: &mut S) -> Option<Request> {
    let mut raw = Vec::new();
    let mut byte = [0u8; 1];
    while !raw.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).await.ok()? == 0 {
            return None;
        }
        raw.push(byte[0]);
    }
    let head = String::from_utf8(raw).ok()?;
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split_whitespace();
    let (method, path) = (first.next()?.to_owned(), first.next()?.to_owned());
    let headers: Vec<(&str, &str)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key, value.trim()))
        .collect();
    let header = |name: &str| {
        headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| (*value).to_owned())
    };
    let len = header("content-length")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let host = header("host").unwrap_or_default();
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).await.ok()?;
    Some(Request {
        method,
        path,
        host,
        body,
    })
}

/// What a handler answers: status, extra headers, body.
type Answer = (u16, Vec<(&'static str, String)>, Vec<u8>);

fn json_answer(status: u16, headers: Vec<(&'static str, String)>, body: &Value) -> Answer {
    (status, headers, body.to_string().into_bytes())
}

fn problem(kind: &str) -> Answer {
    json_answer(
        400,
        Vec::new(),
        &json!({ "type": format!("urn:ietf:params:acme:error:{kind}"), "detail": kind }),
    )
}

impl MockCa {
    fn nonce(&self) -> String {
        let mut state = self.state.lock().unwrap();
        state.next_nonce += 1;
        let nonce = format!("nonce-{}", state.next_nonce);
        state.nonces.insert(nonce.clone());
        nonce
    }

    /// Verify a JWS POST to `url`: its signature, its single-use nonce, its
    /// url, and its key (the JWK on newAccount, the account's kid after).
    /// Returns the payload (`Null` for a POST-as-GET), or the problem.
    fn verify(&self, url: &str, body: &[u8]) -> Result<Value, Answer> {
        let jws: Value = serde_json::from_slice(body).map_err(|_| problem("malformed"))?;
        let field = |name: &str| jws[name].as_str().unwrap_or_default().to_owned();
        let decode = |text: &str| {
            URL_SAFE_NO_PAD
                .decode(text)
                .map_err(|_| problem("malformed"))
        };
        let protected: Value = serde_json::from_slice(&decode(&field("protected"))?)
            .map_err(|_| problem("malformed"))?;
        let mut state = self.state.lock().unwrap();
        let nonce = protected["nonce"].as_str().unwrap_or_default();
        if !state.nonces.remove(nonce) {
            return Err(problem("badNonce"));
        }
        if !state.refused_a_nonce {
            state.refused_a_nonce = true;
            return Err(problem("badNonce"));
        }
        if protected["alg"] != "ES256" || protected["url"] != url {
            return Err(problem("malformed"));
        }
        let key = match (&protected["jwk"], &protected["kid"]) {
            (Value::Object(jwk), Value::Null) => {
                let mut point = vec![4u8];
                point.extend(decode(jwk["x"].as_str().unwrap_or_default())?);
                point.extend(decode(jwk["y"].as_str().unwrap_or_default())?);
                state.account_key = Some(point.clone());
                point
            }
            (Value::Null, Value::String(kid)) if *kid == format!("{}/acct/1", state.base) => state
                .account_key
                .clone()
                .ok_or_else(|| problem("accountDoesNotExist"))?,
            _ => return Err(problem("malformed")),
        };
        let input = format!("{}.{}", field("protected"), field("payload"));
        UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, &key)
            .verify(input.as_bytes(), &decode(&field("signature"))?)
            .map_err(|_| problem("unauthorized"))?;
        if field("payload").is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&decode(&field("payload"))?).map_err(|_| problem("malformed"))
    }

    /// The account's JWK thumbprint, recomputed from its key.
    fn thumbprint(&self) -> String {
        let point = self.state.lock().unwrap().account_key.clone().unwrap();
        let canonical = format!(
            r#"{{"crv":"P-256","kty":"EC","x":"{}","y":"{}"}}"#,
            URL_SAFE_NO_PAD.encode(&point[1..33]),
            URL_SAFE_NO_PAD.encode(&point[33..65]),
        );
        URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(canonical.as_bytes()))
    }

    fn order(&self) -> Value {
        let (base, status, issued) = {
            let state = self.state.lock().unwrap();
            (
                state.base.clone(),
                state.order_status,
                state.chain.is_some(),
            )
        };
        let mut order = json!({
            "status": status,
            "identifiers": [{ "type": "dns", "value": DOMAIN }],
            "authorizations": [format!("{base}/authz/1")],
            "finalize": format!("{base}/finalize/1"),
        });
        if issued {
            order["certificate"] = json!(format!("{base}/cert/1"));
        }
        order
    }

    async fn handle(&self, request: Request) -> Answer {
        // URLs are built from the Host header, as real CAs do, so a client
        // that drops the port gets URLs that lead nowhere.
        let base = format!("https://{}", request.host);
        assert_eq!(
            base,
            self.state.lock().unwrap().base,
            "Host must carry the port"
        );
        let url = format!("{base}{}", request.path);
        if request.method == "GET" {
            return match request.path.as_str() {
                "/dir" => json_answer(
                    200,
                    Vec::new(),
                    &json!({
                        "newNonce": format!("{base}/nonce"),
                        "newAccount": format!("{base}/acct"),
                        "newOrder": format!("{base}/order"),
                    }),
                ),
                "/nonce" => (204, Vec::new(), Vec::new()),
                _ => (404, Vec::new(), Vec::new()),
            };
        }
        let payload = match self.verify(&url, &request.body) {
            Ok(payload) => payload,
            Err(answer) => return answer,
        };
        match request.path.as_str() {
            "/acct" => {
                assert_eq!(payload["termsOfServiceAgreed"], true);
                json_answer(
                    201,
                    vec![("Location", format!("{base}/acct/1"))],
                    &json!({ "status": "valid" }),
                )
            }
            "/order" => {
                assert_eq!(payload["identifiers"][0]["value"], DOMAIN);
                json_answer(
                    201,
                    vec![("Location", format!("{base}/order/1"))],
                    &self.order(),
                )
            }
            "/order/1" => json_answer(200, Vec::new(), &self.order()),
            "/authz/1" => {
                let state = self.state.lock().unwrap();
                json_answer(
                    200,
                    Vec::new(),
                    &json!({
                        "status": state.authz_status,
                        "identifier": { "type": "dns", "value": DOMAIN },
                        "challenges": [
                            { "type": "http-01", "url": format!("{base}/chal/http"), "token": "unused", "status": "pending" },
                            { "type": "tls-alpn-01", "url": format!("{base}/chal/1"), "token": state.token, "status": state.authz_status },
                        ],
                    }),
                )
            }
            "/chal/1" => {
                let valid = self.validate().await;
                self.state.lock().unwrap().authz_status = if valid { "valid" } else { "invalid" };
                json_answer(
                    200,
                    Vec::new(),
                    &json!({ "type": "tls-alpn-01", "status": "processing" }),
                )
            }
            "/finalize/1" => {
                let der = URL_SAFE_NO_PAD
                    .decode(payload["csr"].as_str().unwrap())
                    .unwrap();
                let csr = rcgen::CertificateSigningRequestParams::from_der(&der.into()).unwrap();
                let leaf = csr.signed_by(&self.ca.issuer).unwrap();
                let mut state = self.state.lock().unwrap();
                state.chain = Some(format!("{}{}", leaf.pem(), self.ca.root_pem));
                state.order_status = "valid";
                drop(state);
                json_answer(200, Vec::new(), &self.order())
            }
            "/cert/1" => {
                let chain = self.state.lock().unwrap().chain.clone().unwrap();
                (
                    200,
                    vec![(
                        "Content-Type",
                        "application/pem-certificate-chain".to_owned(),
                    )],
                    chain.into_bytes(),
                )
            }
            _ => (404, Vec::new(), Vec::new()),
        }
    }

    /// TLS-ALPN-01, as a CA does it: dial the name (here, the relay's TCP
    /// ingress) offering only `acme-tls/1`, and check the presented
    /// certificate names the domain and carries the critical acmeIdentifier
    /// extension holding SHA-256 of the key authorization.
    async fn validate(&self) -> bool {
        let token = self.state.lock().unwrap().token.clone();
        let key_authorization = format!("{token}.{}", self.thumbprint());
        let mut expected = vec![0x04, 0x20];
        expected.extend_from_slice(&sha2::Sha256::digest(key_authorization.as_bytes()));

        let seen = Arc::new(Mutex::new(None));
        let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Capture(Arc::clone(&seen))))
        .with_no_client_auth();
        config.alpn_protocols = vec![b"acme-tls/1".to_vec()];
        let tcp = tokio::net::TcpStream::connect(self.ingress).await.unwrap();
        let name = rustls::pki_types::ServerName::try_from(DOMAIN).unwrap();
        let Ok(session) = tokio_rustls::TlsConnector::from(Arc::new(config))
            .connect(name, tcp)
            .await
        else {
            return false;
        };
        if session.get_ref().1.alpn_protocol() != Some(b"acme-tls/1") {
            return false;
        }
        let Some(cert) = seen.lock().unwrap().take() else {
            return false;
        };
        let (_, parsed) = x509_parser::parse_x509_certificate(&cert).unwrap();
        let oid = x509_parser::oid_registry::Oid::from(&[1, 3, 6, 1, 5, 5, 7, 1, 31]).unwrap();
        let carries = parsed
            .extensions()
            .iter()
            .any(|ext| ext.oid == oid && ext.critical && ext.value == expected.as_slice());
        let names = parsed
            .subject_alternative_name()
            .ok()
            .flatten()
            .map(|san| format!("{:?}", san.value))
            .unwrap_or_default();
        carries && names.contains(DOMAIN)
    }
}

/// Accepts any certificate and keeps the leaf, as a TLS-ALPN-01 validator
/// does (the challenge certificate is self-signed by design).
#[derive(Debug)]
struct Capture(Arc<Mutex<Option<Vec<u8>>>>);

impl rustls::client::danger::ServerCertVerifier for Capture {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        *self.0.lock().unwrap() = Some(end_entity.to_vec());
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Serve the mock CA over HTTPS on loopback; returns its base URL.
async fn serve_ca(ca: TestCa, ingress: SocketAddr) -> String {
    let (cert_pem, key_pem) = ca.leaf("localhost");
    let chain = rustls::pki_types::CertificateDer::pem_slice_iter(cert_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let key = rustls::pki_types::PrivateKeyDer::from_pem_slice(key_pem.as_bytes()).unwrap();
    let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(chain, key)
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!(
        "https://localhost:{}",
        listener.local_addr().unwrap().port()
    );
    let mock = Arc::new(MockCa {
        ca,
        state: Mutex::new(CaState {
            base: base.clone(),
            token: "tok3n-abc".to_owned(),
            authz_status: "pending",
            order_status: "pending",
            ..CaState::default()
        }),
        ingress,
    });
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let (acceptor, mock) = (acceptor.clone(), Arc::clone(&mock));
            tokio::spawn(async move {
                let Ok(mut stream) = acceptor.accept(tcp).await else {
                    return;
                };
                let Some(request) = read_request(&mut stream).await else {
                    return;
                };
                let (status, headers, body) = mock.handle(request).await;
                let mut head = format!(
                    "HTTP/1.1 {status} X\r\nReplay-Nonce: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
                    mock.nonce(),
                    body.len()
                );
                for (name, value) in headers {
                    head.push_str(name);
                    head.push_str(": ");
                    head.push_str(&value);
                    head.push_str("\r\n");
                }
                head.push_str("\r\n");
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.write_all(&body).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    base
}

/// A local TCP service: read each connection to its end, then say how much.
async fn counting_target() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut request = Vec::new();
                let _ = stream.read_to_end(&mut request).await;
                let reply = format!("got {} bytes", request.len());
                let _ = stream.write_all(reply.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    addr
}

async fn request(through: SocketAddr, body: &[u8]) -> Vec<u8> {
    let mut stream = tokio::net::TcpStream::connect(through).await.unwrap();
    stream.write_all(body).await.unwrap();
    stream.shutdown().await.unwrap();
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).await.unwrap();
    reply
}

#[tokio::test]
async fn acme_issues_through_the_relay_and_connect_multiplexes_over_it() {
    timeout(DEADLINE, async {
        // The relay, with TCP ingress for the CA's validation and observing
        // every QUIC consumer handshake.
        let relay_dir = tempfile::tempdir().unwrap();
        let mut config = RelayConfig::in_dir(
            "127.0.0.1:0".parse().unwrap(),
            relay_dir.path(),
            Protocol::quictun(),
        );
        config.tcp_ingress = Some(TcpIngress {
            listen: "127.0.0.1:0".parse().unwrap(),
            domain: Some("relay.test".to_owned()),
        });
        let tokens = config.tokens_path.clone();
        let relay_pin = {
            quictun::relay::ensure_self_signed(&config.cert_path, &config.key_path).unwrap();
            cert_fingerprint(&config.cert_path).unwrap()
        };
        let handshakes = Arc::new(Mutex::new(Vec::new()));
        let bound = RelayRuntime::new(config)
            .with_observed_consumer_sni(Arc::clone(&handshakes))
            .bind()
            .unwrap();
        let relay = RelayAddr {
            addr: bound.local_addr(),
            pin: relay_pin,
        };
        let ingress = bound.tcp_addr().unwrap();
        tokio::spawn(bound.serve(std::future::pending()));
        let token = hex::decode(mint_route_token(&tokens, "laptop").unwrap()).unwrap();

        // The service: ACME-managed TLS, forwarding to a local target.
        let certs = Arc::new(AcmeCerts::default());
        let tls = ServiceTls::Terminate(Arc::new(service_config(Arc::clone(&certs), Vec::new())));
        let tunnel = Tunnel::open(&relay, &Protocol::quictun(), &token)
            .await
            .unwrap();
        let target = counting_target().await;
        tokio::spawn(async move { serve_tunnel(&tunnel, target, &tls).await });

        // Issue from the mock CA, which validates through the relay.
        let ca = TestCa::new();
        let roots = Arc::new(ca.roots());
        let directory = format!("{}/dir", serve_ca(ca, ingress).await);
        let service_dir = tempfile::tempdir().unwrap();
        let mut acme = AcmeConfig::new(&directory, DOMAIN, service_dir.path());
        acme.roots = Arc::clone(&roots);
        acme.poll_interval = Duration::from_millis(20);
        issue(&acme, &certs)
            .await
            .expect("issued through the relay");
        assert!(certs.has_certificate());
        assert!(
            service_dir
                .path()
                .join("acme")
                .join(format!("{DOMAIN}.cert.pem"))
                .exists()
        );

        // `connect`, trusting the CA by name, multiplexed.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local = listener.local_addr().unwrap();
        tokio::spawn(forward::connect(
            listener,
            Route {
                relay: relay.clone(),
                name: "laptop".to_owned(),
                alpn: b"quictun/1".to_vec(),
                service: ServiceTrust::Name {
                    name: DOMAIN.to_owned(),
                    roots,
                },
                multiplex: true,
            },
        ));
        let before = handshakes.lock().unwrap().len();
        let replies = futures_join(local).await;
        for (i, reply) in replies.iter().enumerate() {
            assert_eq!(reply, format!("got {} bytes", 10 + i).as_bytes());
        }
        let quic_consumers = handshakes.lock().unwrap().len() - before;
        assert_eq!(quic_consumers, 1, "eight forwards, one QUIC connection");
    })
    .await
    .expect("deadline");
}

/// Eight concurrent requests through `connect`, the i-th `10 + i` bytes.
async fn futures_join(local: SocketAddr) -> Vec<Vec<u8>> {
    let mut tasks = Vec::new();
    // The first dials the shared connection; the rest ride it.
    tasks.push(request(local, &[b'x'; 10]).await);
    let mut rest = tokio::task::JoinSet::new();
    for i in 1..8 {
        rest.spawn(async move { (i, request(local, &vec![b'x'; 10 + i]).await) });
    }
    let mut ordered = vec![Vec::new(); 7];
    while let Some(done) = rest.join_next().await {
        let (i, reply) = done.unwrap();
        ordered[i - 1] = reply;
    }
    tasks.extend(ordered);
    tasks
}
