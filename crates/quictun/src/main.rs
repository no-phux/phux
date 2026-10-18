//! `quictun`: run a blind QUIC tunnel relay, expose a local TCP service
//! through one, or reach an exposed service.

#![forbid(unsafe_code)]
#![allow(
    clippy::print_stdout,
    reason = "stdout carries what the operator copies: addresses, pins, tokens"
)]
#![allow(
    clippy::print_stderr,
    reason = "stderr carries usage and fatal errors; logs go there too"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use quictun::Protocol;
use quictun::connect::RelayAddr;
use quictun::forward::{self, Route, ServiceTls, ServiceTrust};
use quictun::relay::{self, RelayConfig, RelayRuntime, TcpIngress};

const USAGE: &str = "\
quictun: a blind QUIC tunnel relay

USAGE:
  quictun relay   --listen ADDR [--tcp-listen ADDR] [--domain NAME]
                  [--max-conns N] [--max-conns-per-route N] [--state DIR]
  quictun pair    --route NAME [--state DIR]
  quictun expose  --relay HOST:PORT --relay-pin FP --token-file PATH --to ADDR
                  [--cert PEM --key PEM | --acme-domain NAME [--acme-email E]
                   [--acme-directory URL] [--acme-ca PEM] | --passthrough]
                  [--alpn LIST] [--state DIR]
  quictun connect --relay HOST:PORT --relay-pin FP --route NAME --listen ADDR
                  (--service-pin FP | --service-name NAME [--service-ca PEM]
                   | --passthrough)

relay    Serve tunnels and consumers on UDP ADDR. With --tcp-listen, TLS
         clients without QUIC reach route R by the server name R.<--domain>.
pair     Enroll a route (or rotate its token) and print the token and the
         relay's pin. Run it where the relay's state lives.
expose   Dial out to the relay and forward each consumer to the local TCP
         ADDR. TLS ends here, under a certificate kept in --state (or --cert
         and --key); its pin is printed for connect. With --acme-domain, a
         public certificate for NAME comes from Let's Encrypt (or
         --acme-directory) by TLS-ALPN-01 through the relay's TCP ingress,
         which must listen on 443, and is renewed before it expires.
         --passthrough forwards the stream raw to a target that speaks TLS
         itself. --alpn lists the service's ALPNs (http/1.1 with ACME).
connect  Listen on local TCP ADDR and carry every connection to the route
         as a stream on one QUIC connection, pinning the service's
         certificate (--service-pin) or verifying it for NAME against
         Mozilla's roots plus --service-ca (--service-name).

The state directory defaults to $XDG_STATE_HOME/quictun. RUST_LOG sets the
log level (default info).
";

/// Flags that take no value.
const SWITCHES: &[&str] = &["passthrough"];

/// The flags each command takes; anything else is a usage error, so a
/// misspelled flag never runs silently with its default.
fn flags_of(command: &str) -> Option<&'static [&'static str]> {
    Some(match command {
        "relay" => &[
            "listen",
            "tcp-listen",
            "domain",
            "max-conns",
            "max-conns-per-route",
            "state",
        ],
        "pair" => &["route", "state"],
        "expose" => &[
            "relay",
            "relay-pin",
            "token-file",
            "to",
            "cert",
            "key",
            "acme-domain",
            "acme-email",
            "acme-directory",
            "acme-ca",
            "alpn",
            "passthrough",
            "state",
        ],
        "connect" => &[
            "relay",
            "relay-pin",
            "route",
            "listen",
            "service-pin",
            "service-name",
            "service-ca",
            "passthrough",
        ],
        _ => return None,
    })
}

fn main() -> ExitCode {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw
        .iter()
        .any(|arg| matches!(arg.as_str(), "help" | "-h" | "--help"))
    {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let Some((command, rest)) = raw.split_first() else {
        eprint!("{USAGE}");
        return ExitCode::from(2);
    };
    let Some(allowed) = flags_of(command) else {
        return usage_error(&format!("unknown command {command:?}"));
    };
    let args = match Args::parse(rest.iter().cloned(), allowed) {
        Ok(parsed) => parsed,
        Err(err) => return usage_error(&err),
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => return failure(&format!("tokio runtime: {err}")),
    };
    let outcome = runtime.block_on(async {
        match command.as_str() {
            "relay" => run_relay(&args).await,
            "pair" => run_pair(&args),
            "expose" => run_expose(&args).await,
            _ => run_connect(&args).await,
        }
    });
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::Usage(err)) => usage_error(&err),
        Err(Failure::Run(err)) => failure(&err),
    }
}

fn usage_error(err: &str) -> ExitCode {
    eprintln!("quictun: {err}\n\n{USAGE}");
    ExitCode::from(2)
}

fn failure(err: &str) -> ExitCode {
    eprintln!("quictun: {err}");
    ExitCode::FAILURE
}

/// Why a command stopped: bad arguments, or a failure while running.
enum Failure {
    Usage(String),
    Run(String),
}

/// Map any displayable error to a run failure with context.
fn run<E: std::fmt::Display>(context: &str) -> impl FnOnce(E) -> Failure + '_ {
    move |err| Failure::Run(format!("{context}: {err}"))
}

/// `--flag value` pairs and bare switches, each given at most once.
struct Args {
    values: BTreeMap<String, String>,
    switches: BTreeSet<String>,
}

impl Args {
    fn parse(argv: impl Iterator<Item = String>, allowed: &[&str]) -> Result<Self, String> {
        let mut values = BTreeMap::new();
        let mut switches = BTreeSet::new();
        let mut argv = argv.peekable();
        while let Some(arg) = argv.next() {
            let Some(flag) = arg.strip_prefix("--") else {
                return Err(format!("unexpected argument {arg:?}"));
            };
            if !allowed.contains(&flag) {
                return Err(format!("unknown flag --{flag}"));
            }
            if SWITCHES.contains(&flag) {
                switches.insert(flag.to_owned());
                continue;
            }
            let Some(value) = argv.next_if(|next| !next.starts_with("--")) else {
                return Err(format!("--{flag} needs a value"));
            };
            if values.insert(flag.to_owned(), value).is_some() {
                return Err(format!("--{flag} given twice"));
            }
        }
        Ok(Self { values, switches })
    }

    fn optional(&self, flag: &str) -> Option<&str> {
        self.values.get(flag).map(String::as_str)
    }

    fn required(&self, flag: &str) -> Result<&str, Failure> {
        self.optional(flag)
            .ok_or_else(|| Failure::Usage(format!("--{flag} is required")))
    }

    fn parsed<T: std::str::FromStr>(&self, flag: &str) -> Result<Option<T>, Failure> {
        self.optional(flag)
            .map(|raw| {
                raw.parse()
                    .map_err(|_| Failure::Usage(format!("--{flag}: cannot parse {raw:?}")))
            })
            .transpose()
    }

    /// A count that must be at least one: zero would refuse everything, or
    /// be silently raised.
    fn positive(&self, flag: &str) -> Result<Option<usize>, Failure> {
        match self.parsed::<usize>(flag)? {
            Some(0) => Err(Failure::Usage(format!("--{flag} must be at least 1"))),
            count => Ok(count),
        }
    }

    fn switch(&self, flag: &str) -> bool {
        self.switches.contains(flag)
    }

    fn state_dir(&self) -> PathBuf {
        self.optional("state")
            .map_or_else(|| relay::state_dir("quictun"), PathBuf::from)
    }
}

async fn run_relay(args: &Args) -> Result<(), Failure> {
    let listen = args
        .parsed::<SocketAddr>("listen")?
        .ok_or_else(|| Failure::Usage("--listen is required".to_owned()))?;
    let mut config = RelayConfig::in_dir(listen, &args.state_dir(), Protocol::quictun());
    if let Some(max_conns) = args.positive("max-conns")? {
        config.max_conns = max_conns;
    }
    config.max_conns_per_route = args.positive("max-conns-per-route")?;
    if args.optional("domain").is_some() && args.optional("tcp-listen").is_none() {
        return Err(Failure::Usage("--domain needs --tcp-listen".to_owned()));
    }
    config.tcp_ingress = args
        .parsed::<SocketAddr>("tcp-listen")?
        .map(|listen| TcpIngress {
            listen,
            domain: args.optional("domain").map(str::to_owned),
        });
    let cert_path = config.cert_path.clone();
    let bound = RelayRuntime::new(config).bind().map_err(run("relay"))?;
    let pin = relay::cert_fingerprint(&cert_path).map_err(run("relay certificate"))?;
    println!("relay:     {} (udp)", bound.local_addr());
    if let Some(tcp) = bound.tcp_addr() {
        println!("ingress:   {tcp} (tcp)");
    }
    println!("relay pin: {pin}");
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    bound.serve(shutdown).await.map_err(run("relay"))
}

fn run_pair(args: &Args) -> Result<(), Failure> {
    let route = args.required("route")?;
    // Before anything is written: a bad name must leave no state behind.
    relay::validate_route_name(route).map_err(run("pair"))?;
    let config = RelayConfig::in_dir(
        SocketAddr::from(([0, 0, 0, 0], 0)),
        &args.state_dir(),
        Protocol::quictun(),
    );
    relay::ensure_self_signed(&config.cert_path, &config.key_path)
        .map_err(run("relay certificate"))?;
    let token = relay::mint_route_token(&config.tokens_path, route).map_err(run("pair"))?;
    let pin = relay::cert_fingerprint(&config.cert_path).map_err(run("relay certificate"))?;
    println!("route:     {route}");
    println!("token:     {token}");
    println!("relay pin: {pin}");
    println!();
    println!("Save the token in a file only you can read (chmod 600) and pass it");
    println!("to `quictun expose --token-file`. It is shown once; pairing again");
    println!("rotates it.");
    Ok(())
}

async fn run_expose(args: &Args) -> Result<(), Failure> {
    let (tls, acme) = service_tls(args)?;
    let relay = relay_addr(args).await?;
    let token = read_token(Path::new(args.required("token-file")?))?;
    let target = resolve(args.required("to")?).await?;
    // Issuance validates through the tunnel, so it runs beside it.
    if let Some((config, certs)) = acme {
        tokio::spawn(quictun::acme::maintain(config, certs));
    }
    forward::expose(&relay, &Protocol::quictun(), &token, target, tls)
        .await
        .map_err(run("the relay refused the token"))
}

async fn run_connect(args: &Args) -> Result<(), Failure> {
    let relay = relay_addr(args).await?;
    let name = args.required("route")?.to_owned();
    let service = service_trust(args)?;
    let listen = args
        .parsed::<SocketAddr>("listen")?
        .ok_or_else(|| Failure::Usage("--listen is required".to_owned()))?;
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(run("listen"))?;
    println!(
        "listening: {}",
        listener.local_addr().map_err(run("listen"))?
    );
    let alpn = Protocol::quictun().consumer_alpns.swap_remove(0);
    let route = Route {
        relay,
        name,
        alpn,
        service,
        // The quictun relay bridges every stream: one connection, one
        // handshake, for every local connection.
        multiplex: true,
    };
    forward::connect(listener, route).await;
    Ok(())
}

/// How `connect` trusts the service: exactly one of `--service-pin`,
/// `--service-name` (with optional extra CAs from `--service-ca`), and
/// `--passthrough`.
fn service_trust(args: &Args) -> Result<ServiceTrust, Failure> {
    let pin = args.optional("service-pin");
    let name = args.optional("service-name");
    let passthrough = args.switch("passthrough");
    if usize::from(pin.is_some()) + usize::from(name.is_some()) + usize::from(passthrough) != 1 {
        return Err(Failure::Usage(
            "give exactly one of --service-pin, --service-name, and --passthrough".to_owned(),
        ));
    }
    if args.optional("service-ca").is_some() && name.is_none() {
        return Err(Failure::Usage(
            "--service-ca needs --service-name".to_owned(),
        ));
    }
    if let Some(pin) = pin {
        return Ok(ServiceTrust::Pin(pin.to_owned()));
    }
    let Some(name) = name else {
        return Ok(ServiceTrust::Passthrough);
    };
    let mut roots = forward::web_roots();
    if let Some(ca) = args.optional("service-ca") {
        for cert in quictun::cert::load_certs(Path::new(ca)).map_err(run("--service-ca"))? {
            roots.add(cert).map_err(run("--service-ca"))?;
        }
    }
    Ok(ServiceTrust::Name {
        name: name.to_owned(),
        roots: Arc::new(roots),
    })
}

/// The relay's address and pin, from `--relay` and `--relay-pin`.
async fn relay_addr(args: &Args) -> Result<RelayAddr, Failure> {
    Ok(RelayAddr {
        addr: resolve(args.required("relay")?).await?,
        pin: args.required("relay-pin")?.to_owned(),
    })
}

/// The first address `HOST:PORT` resolves to.
async fn resolve(target: &str) -> Result<SocketAddr, Failure> {
    tokio::net::lookup_host(target)
        .await
        .map_err(run(target))?
        .next()
        .ok_or_else(|| Failure::Run(format!("{target}: resolves to no address")))
}

/// The route token from an owner-only file, hex-encoded.
fn read_token(path: &Path) -> Result<Vec<u8>, Failure> {
    use quictun::secret_file::{SecretFile, check};
    if let Some(warning) = check(path, SecretFile::OwnerOnlyToken).map_err(Failure::Run)? {
        tracing::warn!("{warning}");
    }
    let raw = std::fs::read_to_string(path).map_err(run(&path.display().to_string()))?;
    hex::decode(raw.trim())
        .map_err(|_| Failure::Run(format!("{}: the token is not hex", path.display())))
}

/// The service end's TLS, printing the pin `connect` needs.
fn service_tls(args: &Args) -> Result<(ServiceTls, Option<Acme>), Failure> {
    let has = |flags: &[&str]| flags.iter().any(|flag| args.optional(flag).is_some());
    let acme_flags = ["acme-email", "acme-directory", "acme-ca"];
    if has(&acme_flags) && !has(&["acme-domain"]) {
        return Err(Failure::Usage(
            "--acme-email, --acme-directory, and --acme-ca need --acme-domain".to_owned(),
        ));
    }
    if args.switch("passthrough") {
        if has(&["cert", "key", "acme-domain", "alpn"]) {
            return Err(Failure::Usage(
                "--passthrough forwards TLS untouched; it takes no --cert, --key, --acme-domain, or --alpn"
                    .to_owned(),
            ));
        }
        return Ok((ServiceTls::Passthrough, None));
    }
    if let Some(domain) = args.optional("acme-domain") {
        if has(&["cert", "key"]) {
            return Err(Failure::Usage(
                "--acme-domain obtains the certificate; it takes no --cert or --key".to_owned(),
            ));
        }
        return acme_tls(args, domain);
    }
    let alpns = alpns(args, &[]);
    let (cert, key) = match (args.optional("cert"), args.optional("key")) {
        (Some(cert), Some(key)) => (PathBuf::from(cert), PathBuf::from(key)),
        (None, None) => {
            let dir = args.state_dir();
            let (cert, key) = (dir.join("service-cert.pem"), dir.join("service-key.pem"));
            quictun::cert::ensure_self_signed(&cert, &key).map_err(run("service certificate"))?;
            (cert, key)
        }
        _ => return Err(Failure::Usage("give --cert and --key together".to_owned())),
    };
    let config =
        forward::service_tls_config(&cert, &key, alpns).map_err(run("service certificate"))?;
    let pin = quictun::cert::cert_fingerprint(&cert).map_err(run("service certificate"))?;
    println!("service pin: {pin}");
    Ok((ServiceTls::Terminate(Arc::new(config)), None))
}

/// An ACME-maintained certificate and where it comes from.
type Acme = (quictun::acme::AcmeConfig, Arc<quictun::acme::AcmeCerts>);

/// TLS under a certificate ACME obtains for `domain` and keeps renewed.
fn acme_tls(args: &Args, domain: &str) -> Result<(ServiceTls, Option<Acme>), Failure> {
    use quictun::acme::{AcmeCerts, AcmeConfig, LETS_ENCRYPT, service_config};
    let directory = args.optional("acme-directory").unwrap_or(LETS_ENCRYPT);
    let mut config = AcmeConfig::new(directory, domain, &args.state_dir());
    config.contact = args.optional("acme-email").map(str::to_owned);
    if let Some(ca) = args.optional("acme-ca") {
        let mut roots = forward::web_roots();
        for cert in quictun::cert::load_certs(Path::new(ca)).map_err(run("--acme-ca"))? {
            roots.add(cert).map_err(run("--acme-ca"))?;
        }
        config.roots = Arc::new(roots);
    }
    let certs = Arc::new(AcmeCerts::default());
    // A web service by default: a browser offering h2 and http/1.1 needs
    // one it shares, or the handshake fails.
    let tls = service_config(Arc::clone(&certs), alpns(args, &["http/1.1"]));
    println!("service name: {domain} (ACME, from {directory})");
    Ok((ServiceTls::Terminate(Arc::new(tls)), Some((config, certs))))
}

/// The service's ALPNs from `--alpn a,b`, else `default`.
fn alpns(args: &Args, default: &[&str]) -> Vec<Vec<u8>> {
    args.optional("alpn").map_or_else(
        || {
            default
                .iter()
                .map(|alpn| alpn.as_bytes().to_vec())
                .collect()
        },
        |list| {
            list.split(',')
                .map(str::trim)
                .filter(|alpn| !alpn.is_empty())
                .map(|alpn| alpn.as_bytes().to_vec())
                .collect()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(command: &str, args: &[&str]) -> Result<Args, String> {
        let allowed = flags_of(command).expect("known command");
        Args::parse(args.iter().map(|arg| (*arg).to_owned()), allowed)
    }

    #[test]
    fn flags_take_values_and_switches_do_not() {
        let args = parse(
            "expose",
            &["--relay", "r:1", "--passthrough", "--to", "127.0.0.1:80"],
        )
        .unwrap();
        assert_eq!(args.optional("relay"), Some("r:1"));
        assert_eq!(args.optional("to"), Some("127.0.0.1:80"));
        assert!(args.switch("passthrough"));
        assert!(parse("pair", &["--route"]).is_err(), "missing value");
        assert!(
            parse("expose", &["--relay", "--to", "x"]).is_err(),
            "flag as value"
        );
        assert!(parse("pair", &["laptop"]).is_err(), "positional");
        assert!(
            parse("pair", &["--route", "a", "--route", "b"]).is_err(),
            "twice"
        );
    }

    /// A flag another command takes, or a misspelling, is refused rather
    /// than silently ignored.
    #[test]
    fn each_command_takes_only_its_own_flags() {
        assert!(
            parse("relay", &["--max-conns-per-rout", "4"]).is_err(),
            "typo"
        );
        assert!(
            parse("pair", &["--listen", "0.0.0.0:1"]).is_err(),
            "relay's flag"
        );
        assert!(
            parse("connect", &["--state", "/tmp"]).is_err(),
            "connect keeps no state"
        );
        assert!(flags_of("frobnicate").is_none());
    }

    #[test]
    fn counts_must_be_positive() {
        let zero = parse("relay", &["--max-conns", "0"]).unwrap();
        assert!(zero.positive("max-conns").is_err());
        let four = parse("relay", &["--max-conns", "4"]).unwrap();
        assert!(matches!(four.positive("max-conns"), Ok(Some(4))));
    }
}
