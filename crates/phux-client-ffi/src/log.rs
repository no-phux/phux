//! Process-wide diagnostics for embedders: one `tracing` subscriber on
//! standard error, installed once per process.
//!
//! The bridge never chooses a log path. An embedder that wants a file
//! redirects descriptor 2 before calling [`phux_client_log_init`], so the
//! bridge's lines land beside the host's own and beside whatever a fatal
//! signal handler prints. The filter follows `RUST_LOG` grammar: the caller's
//! directives when given, else the `RUST_LOG` environment variable, else
//! [`DEFAULT_FILTER`], which records the remote tunnel's lifecycle at info and
//! leaves the transport stacks (`quinn`, `rustls`) at warn.

use std::sync::atomic::{AtomicBool, Ordering};

use tracing_subscriber::EnvFilter;

use crate::error::BridgeError;
use crate::remote::{guard, text_in};
use crate::types::{PhuxBytes, PhuxClientResult};

/// Directives applied when neither the caller nor `RUST_LOG` names any.
pub const DEFAULT_FILTER: &str =
    "phux=info,phux_client_ffi=info,phux_dial=info,quinn=warn,rustls=warn,warn";

/// The environment variable consulted when the caller's span is empty.
const ENV_FILTER: &str = "RUST_LOG";

/// A filter is a directive list, not a document.
const MAX_FILTER_BYTES: usize = 4096;

/// Set once the process has decided its subscriber, whether this bridge
/// installed it or found one already in place.
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Install the bridge's `tracing` subscriber on standard error.
///
/// Idempotent per process: once a subscriber is in place, every later call
/// with a parsable filter returns `Ok` and changes nothing. `filter` is a
/// `RUST_LOG`-style directive list; an empty span means the `RUST_LOG`
/// environment variable, else [`DEFAULT_FILTER`]. An unparsable filter
/// returns `InvalidArgument` and installs nothing, on every call. When
/// another subscriber is already global (an embedder that links its own
/// Rust), the call returns `Ok` and leaves it in charge.
///
/// # Safety
///
/// A non-empty `filter` must be readable for `filter.len` bytes for the
/// duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_log_init(filter: PhuxBytes) -> PhuxClientResult {
    guard(|| {
        // SAFETY: forwarded caller contract.
        let requested = unsafe { text_in(filter, MAX_FILTER_BYTES, "filter") }?;
        let env = std::env::var(ENV_FILTER).ok();
        let (filter, directives) = resolve_filter(requested, env.as_deref())?;
        install(filter, directives);
        Ok(())
    })
}

/// The directives a call resolves to, parsed: the caller's text when
/// non-empty, else a non-empty `RUST_LOG`, else [`DEFAULT_FILTER`].
fn resolve_filter<'a>(
    requested: &'a str,
    env: Option<&'a str>,
) -> Result<(EnvFilter, &'a str), BridgeError> {
    let directives = if requested.is_empty() {
        env.filter(|value| !value.is_empty())
            .unwrap_or(DEFAULT_FILTER)
    } else {
        requested
    };
    EnvFilter::try_new(directives)
        .map(|filter| (filter, directives))
        .map_err(|err| {
            BridgeError::invalid(format!("filter is not a RUST_LOG directive list: {err}"))
        })
}

/// Install the subscriber unless the process already has one. Two threads
/// racing here both mark the process decided; only one `try_init` succeeds.
fn install(filter: EnvFilter, directives: &str) {
    if INSTALLED.load(Ordering::Acquire) {
        return;
    }
    let installed = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_target(true)
        .with_thread_names(true)
        .try_init()
        .is_ok();
    INSTALLED.store(true, Ordering::Release);
    if installed {
        tracing::info!(
            version = env!("CARGO_PKG_VERSION"),
            filter = directives,
            "phux client bridge logging started"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(text: &str) -> PhuxBytes {
        PhuxBytes {
            data: text.as_ptr(),
            len: text.len(),
        }
    }

    #[test]
    fn the_default_filter_parses_and_is_the_last_fallback() {
        let (_, chosen) = resolve_filter("", None).expect("default parses");
        assert_eq!(chosen, DEFAULT_FILTER);
        let (_, chosen) = resolve_filter("", Some("")).expect("empty env is unset");
        assert_eq!(chosen, DEFAULT_FILTER);
        let (_, chosen) = resolve_filter("", Some("quinn=trace")).expect("env parses");
        assert_eq!(chosen, "quinn=trace");
        let (_, chosen) = resolve_filter("phux=debug", Some("quinn=trace")).expect("caller wins");
        assert_eq!(chosen, "phux=debug");
    }

    #[test]
    fn a_second_init_is_a_no_op_that_succeeds() {
        // SAFETY: the spans borrow string literals for the call.
        let first = unsafe { phux_client_log_init(span("")) };
        let second = unsafe { phux_client_log_init(span("phux=trace")) };
        assert_eq!(first, PhuxClientResult::Ok);
        assert_eq!(second, PhuxClientResult::Ok);
        assert!(INSTALLED.load(Ordering::Acquire));
    }

    #[test]
    fn an_unparsable_filter_is_refused_and_installs_nothing() {
        let before = INSTALLED.load(Ordering::Acquire);
        // SAFETY: the span borrows a string literal for the call.
        let result = unsafe { phux_client_log_init(span("phux=notalevel[[")) };
        assert_eq!(result, PhuxClientResult::InvalidArgument);
        assert_eq!(INSTALLED.load(Ordering::Acquire), before);
        // NUL and oversized spans are argument errors too, never a parse.
        // SAFETY: the span borrows a string literal for the call.
        let nul = unsafe { phux_client_log_init(span("phux=info\0")) };
        assert_eq!(nul, PhuxClientResult::InvalidArgument);
        assert_eq!(INSTALLED.load(Ordering::Acquire), before);
    }
}
