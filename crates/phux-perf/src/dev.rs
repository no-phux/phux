//! Dev mode: one switch, `PHUX_DEV=1`, read the same way by every process.
//!
//! The always-on histograms and counters do not need it. Dev mode adds what
//! costs something: heap accounting through [`crate::alloc`] (when the binary
//! installs the counting allocator), and, in the desktop, the dev socket and
//! the perf HUD. Span capture through [`crate::trace`] is armed on demand and
//! does not need dev mode either; it only needs someone to ask.

/// The environment variable that turns dev mode on.
pub const ENV: &str = "PHUX_DEV";

/// Does `value` turn dev mode on? `1`, `true`, `on` and `yes`, any case.
#[must_use]
pub fn is_on(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "on" | "yes"
    )
}

/// Is dev mode requested in this process's environment?
#[must_use]
pub fn requested() -> bool {
    std::env::var(ENV).is_ok_and(|value| is_on(&value))
}

/// Apply dev mode from the environment once at startup: turn on heap
/// accounting when it is requested. Returns whether it was, for the caller
/// to log.
#[must_use = "log whether dev mode came on"]
pub fn init_from_env() -> bool {
    let on = requested();
    if on {
        crate::alloc::enable();
    }
    on
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spellings_of_on() {
        for on in ["1", "true", "ON", " yes "] {
            assert!(is_on(on), "{on:?}");
        }
        for off in ["", "0", "false", "off", "dev"] {
            assert!(!is_on(off), "{off:?}");
        }
    }
}
