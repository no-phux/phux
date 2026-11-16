//! The context a request/response verb runs in: its runtime, the server it
//! talks to, and the channel its failures report on.
//!
//! A verb body returns `Result<ExitCode, Refusal>` and uses `?` on the
//! parse, resolve, and transport helpers; [`Ctx::finish`] renders a refusal
//! once, as one JSON line under `--json` or the verb's prose otherwise. `Ok`
//! carries any status the verb decided itself, including a failure it has
//! already reported (a print-then-fail, a declined confirmation), so verbs
//! keep owning their own output and their special exits.
//!
//! The verbs that own a runtime of their own (`attach`, `server`, `play`,
//! `rec`, streaming `watch`) do not use it.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_client::attach::AttachError;
use phux_config::socket::default_socket_path;
use tokio::runtime::Runtime;

use crate::commands::json_err::Refusal;
use crate::commands::server_target::{ServerSpec, ServerTarget};

/// A verb's runtime, its server, and whether it answers in JSON. `T` is the
/// server: a [`ServerTarget`] for the verbs that take `--remote`, the local
/// socket path for the rest.
pub(crate) struct Ctx<T = ServerTarget> {
    pub(crate) rt: Runtime,
    pub(crate) target: T,
    pub(crate) json: bool,
}

impl Ctx<PathBuf> {
    /// The context of a verb that only ever talks to the local server: the
    /// `--socket` override, else the default socket.
    pub(crate) fn local(socket: Option<PathBuf>, json: bool) -> Result<Self, ExitCode> {
        Ok(Self {
            rt: crate::commands::cli_runtime()?,
            target: socket.unwrap_or_else(default_socket_path),
            json,
        })
    }

    /// The local socket this verb dials.
    pub(crate) fn socket(&self) -> &Path {
        &self.target
    }

    /// The refusal for a failed exchange with the local server.
    pub(crate) fn unreachable(&self, err: &AttachError, verb: &str) -> Refusal {
        Refusal::no_server(err, &self.target, verb)
    }
}

impl Ctx<ServerTarget> {
    /// Build the verb's runtime and resolve the server `spec` names,
    /// reporting a resolution refusal itself.
    pub(crate) fn prepare(spec: ServerSpec, verb: &str, json: bool) -> Result<Self, ExitCode> {
        let (rt, target) = spec.prepare(verb, json)?;
        Ok(Self { rt, target, json })
    }

    /// The refusal for a failed exchange with this server.
    pub(crate) fn unreachable(&self, err: &AttachError, verb: &str) -> Refusal {
        self.target.classify(err, verb)
    }
}

impl<T> Ctx<T> {
    /// The verb's exit: its own status, or its refusal reported on its
    /// channel.
    pub(crate) fn finish(&self, result: Result<ExitCode, Refusal>) -> ExitCode {
        result.unwrap_or_else(|refusal| refusal.emit(self.json))
    }
}

/// Run a local verb's `body` in its context and render the outcome.
pub(crate) fn run_local(
    socket: Option<PathBuf>,
    json: bool,
    body: impl FnOnce(&Ctx<PathBuf>) -> Result<ExitCode, Refusal>,
) -> ExitCode {
    match Ctx::local(socket, json) {
        Ok(ctx) => ctx.finish(body(&ctx)),
        Err(code) => code,
    }
}

/// Run a `--remote`-capable verb's `body` against the server `spec` names.
pub(crate) fn run(
    spec: ServerSpec,
    verb: &str,
    json: bool,
    body: impl FnOnce(&Ctx) -> Result<ExitCode, Refusal>,
) -> ExitCode {
    match Ctx::prepare(spec, verb, json) {
        Ok(ctx) => ctx.finish(body(&ctx)),
        Err(code) => code,
    }
}
