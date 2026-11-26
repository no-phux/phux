//! Key modes: `enter-mode` and `exit-mode` switch the resolver's active
//! mode; the status bar's chip follows it after the batch.

use crate::attach::input_dispatch::args::str_arg;

use super::ActionCall;

/// `enter-mode { name, once? }`: make `name` the active key mode. Bells when
/// no such mode exists.
pub(in crate::attach) fn enter_mode(
    ActionCall {
        resolved,
        ctx,
        effects,
        ..
    }: ActionCall<'_, '_>,
) {
    let once = resolved
        .args
        .get("once")
        .and_then(toml::Value::as_bool)
        .unwrap_or(false);
    let entered =
        str_arg(resolved, "name").is_some_and(|name| ctx.resolver.enter_mode(&name, once));
    if entered {
        tracing::debug!(args = ?resolved.args, "key mode entered");
    } else {
        tracing::warn!(args = ?resolved.args, "enter-mode names no key mode");
        effects.bell = true;
    }
}

/// `exit-mode`: leave the active key mode; a no-op outside one.
pub(in crate::attach) fn exit_mode(ActionCall { ctx, .. }: ActionCall<'_, '_>) {
    if ctx.resolver.exit_mode() {
        tracing::debug!("key mode left");
    }
}
