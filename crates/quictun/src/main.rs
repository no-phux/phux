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
use quictun::forward::{self, Route, ServiceTls};
use quictun::relay::{self, RelayConfig, RelayRuntime, TcpIngress};

const USAGE: &str = "\
quictun: a blind QUIC tunnel relay

USAGE:
  quictun relay   --listen ADDR [--tcp-listen ADDR] [--domain NAME]
                  [--max-conns N] [--max-conns-per-route N] [--state DIR]
  quictun pair    --route NAME [--state DIR]
  quictun expose  --relay HOST:PORT --relay-pin FP --token-file PATH --to ADDR
                  [--cert PEM --key PEM | --passthrough] [--state DIR]
  quictun connect --relay HOST:PORT --relay-pin FP --route NAME --listen ADDR
                  (--service-pin FP | --passthrough)

relay    Serve tunnels and consumers on UDP ADDR. With --tcp-listen, TLS
         clients without QUIC reach route R by the server name R.<--domain>.
pair     Enroll a route (or rotate its token) and print the token and the
         relay's pin. Run it where the relay's state lives.
expose   Dial out to the relay and forward each consumer to the local TCP
         ADDR. TLS ends here, under a certificate kept in --state (or --cert
         and --key); its pin is printed for connect. --passthrough forwards
         the stream raw to a target that speaks TLS itself.
connect  Listen on local TCP ADDR and carry each connection to the route,
         pinning the service's certificate.

The state directory defaults to $XDG_STATE_HOME/quictun. RUST_LOG sets the
log level (default info).
";

/// Flags that take no value.
const SWITCHES: &[&str] = &["passthrough"];

fn main() -> ExitCode {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
    let mut raw = std::env::args().skip(1);
    let Some(command) = raw.next() else {
        eprint!("{USAGE}");
        return ExitCode::from(2);
    };
    if matches!(command.as_str(), "help" | "-h" | "--help") {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let args = match Args::parse(raw) {
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
            "connect" => run_connect(&args).await,
            other => Err(Failure::Usage(format!("unknown command {other:?}"))),
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
    fn parse(argv: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut values = BTreeMap::new();
        let mut switches = BTreeSet::new();
        let mut argv = argv.peekable();
        while let Some(arg) = argv.next() {
            let Some(flag) = arg.strip_prefix("--") else {
                return Err(format!("unexpected argument {arg:?}"));
            };
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
    if let Some(max_conns) = args.parsed("max-conns")? {
        config.max_conns = max_conns;
    }
    config.max_conns_per_route = args.parsed("max-conns-per-route")?;
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
    let relay = relay_addr(args).await?;
    let token = read_token(Path::new(args.required("token-file")?))?;
    let target = resolve(args.required("to")?).await?;
    let tls = service_tls(args)?;
    forward::expose(&relay, &Protocol::quictun(), &token, target, tls)
        .await
        .map_err(run("the relay refused the token"))
}

async fn run_connect(args: &Args) -> Result<(), Failure> {
    let relay = relay_addr(args).await?;
    let name = args.required("route")?.to_owned();
    let service_pin = match (args.optional("service-pin"), args.switch("passthrough")) {
        (Some(pin), false) => Some(pin.to_owned()),
        (None, true) => None,
        _ => {
            return Err(Failure::Usage(
                "give exactly one of --service-pin and --passthrough".to_owned(),
            ));
        }
    };
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
        service_pin,
    };
    forward::connect(listener, route)
        .await
        .map_err(run("accept"))
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
fn service_tls(args: &Args) -> Result<ServiceTls, Failure> {
    if args.switch("passthrough") {
        return Ok(ServiceTls::Passthrough);
    }
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
        forward::service_tls_config(&cert, &key, Vec::new()).map_err(run("service certificate"))?;
    let pin = quictun::cert::cert_fingerprint(&cert).map_err(run("service certificate"))?;
    println!("service pin: {pin}");
    Ok(ServiceTls::Terminate(Arc::new(config)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, String> {
        Args::parse(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn flags_take_values_and_switches_do_not() {
        let args = parse(&["--route", "laptop", "--passthrough", "--to", "127.0.0.1:80"]).unwrap();
        assert_eq!(args.optional("route"), Some("laptop"));
        assert_eq!(args.optional("to"), Some("127.0.0.1:80"));
        assert!(args.switch("passthrough"));
        assert!(parse(&["--route"]).is_err(), "missing value");
        assert!(parse(&["--route", "--to", "x"]).is_err(), "flag as value");
        assert!(parse(&["laptop"]).is_err(), "positional");
        assert!(parse(&["--route", "a", "--route", "b"]).is_err(), "twice");
    }
}
