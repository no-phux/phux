//! The signal names `phux_signal` accepts and the wire signal each selects.

use phux_protocol::wire::frame::TerminalSignal;

/// Every signal `phux_signal` accepts, by its argument name: the one table
/// the input schema, the validator and both name lookups read.
const SIGNALS: [(&str, TerminalSignal); 5] = [
    ("interrupt", TerminalSignal::Interrupt),
    ("freeze", TerminalSignal::Freeze),
    ("resume", TerminalSignal::Resume),
    ("terminate", TerminalSignal::Terminate),
    ("kill", TerminalSignal::Kill),
];

/// The accepted signal names, in schema order.
pub(crate) fn signal_names() -> [&'static str; SIGNALS.len()] {
    SIGNALS.map(|(name, _)| name)
}

/// The wire signal an argument name selects, if it is one.
pub(crate) fn signal_named(name: &str) -> Option<TerminalSignal> {
    SIGNALS
        .iter()
        .find(|(known, _)| *known == name)
        .map(|&(_, signal)| signal)
}
