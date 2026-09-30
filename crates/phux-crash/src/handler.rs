//! Fatal-signal handler that restores the terminal and re-raises.
//!
//! MODIFIED FROM UPSTREAM (`xai-crash-handler`): only the terminal-restore
//! handlers are kept. Upstream's crash-blob writer, frame-pointer walker, and
//! Windows exception filters are removed — see the crate NOTICE.
//!
//! Everything reachable from a handler is async-signal-safe: raw `write(2)`,
//! `tcsetattr(3)`, `sigaction(2)`, and `raise(3)` over statics initialised
//! before the handler is registered. No allocation, no locks.

#[cfg(unix)]
mod imp {
    use std::sync::atomic::{AtomicBool, Ordering};

    use crate::terminal;

    /// Saved original terminal state for restoration in the signal handler.
    // SAFETY: `termios` is a plain C struct of integers and arrays, for which
    // all-zero bytes is a valid value.
    static mut ORIGINAL_TERMIOS: libc::termios = unsafe { std::mem::zeroed() };

    /// Whether we successfully saved the original termios.
    static mut HAS_TERMIOS: bool = false;

    /// Alternate signal stack memory (16 KiB via mmap).
    const ALT_STACK_SIZE: usize = 16 * 1024;

    /// Guards against allocating the alternate signal stack more than once
    /// when [`install_terminal_restore_only`] runs on every attach.
    static ALT_STACK_INSTALLED: AtomicBool = AtomicBool::new(false);

    /// Save the current terminal state for restoration in signal handlers.
    fn save_termios() {
        // SAFETY: runs on the attaching thread before the handler reads these
        // statics (the handler is registered after this returns), and
        // `tcgetattr` only writes into the `termios` we pass it.
        unsafe {
            let termios = &mut *std::ptr::addr_of_mut!(ORIGINAL_TERMIOS);
            if libc::tcgetattr(0, termios) == 0 {
                *std::ptr::addr_of_mut!(HAS_TERMIOS) = true;
            }
        }
    }

    /// Allocate an alternate signal stack via mmap (survives stack overflow).
    ///
    /// No-op if already installed.
    fn setup_alt_stack() {
        if ALT_STACK_INSTALLED.swap(true, Ordering::AcqRel) {
            return;
        }
        // SAFETY: an anonymous private mapping has no aliasing preconditions,
        // and `sigaltstack` is given a fully initialised `stack_t` pointing at
        // that mapping only when `mmap` succeeded. The mapping is never
        // unmapped, so the stack outlives every handler invocation.
        unsafe {
            let stack_mem = libc::mmap(
                std::ptr::null_mut(),
                ALT_STACK_SIZE,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            );
            if stack_mem != libc::MAP_FAILED {
                let ss = libc::stack_t {
                    ss_sp: stack_mem,
                    ss_flags: 0,
                    ss_size: ALT_STACK_SIZE,
                };
                libc::sigaltstack(&ss, std::ptr::null_mut());
            }
        }
    }

    /// Restore termios and re-raise. No escape codes.
    ///
    /// # Safety
    ///
    /// Must only be called from a signal handler context.
    unsafe fn restore_termios_and_reraise(sig: libc::c_int) {
        // SAFETY: `tcsetattr`, `sigemptyset`, `sigaction`, and `raise` are all
        // async-signal-safe; the termios statics were written before the
        // handler was registered and are only read here.
        unsafe {
            if *std::ptr::addr_of!(HAS_TERMIOS) {
                libc::tcsetattr(0, libc::TCSANOW, std::ptr::addr_of!(ORIGINAL_TERMIOS));
            }
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = libc::SIG_DFL;
            sa.sa_flags = 0;
            libc::sigemptyset(&mut sa.sa_mask);
            libc::sigaction(sig, &sa, std::ptr::null_mut());
            libc::raise(sig);
        }
    }

    /// Restore terminal escape codes + termios, then re-raise.
    ///
    /// # Safety
    ///
    /// Must only be called from a signal handler context.
    unsafe fn restore_terminal_and_reraise(sig: libc::c_int) {
        terminal::restore_in_signal_handler();
        // SAFETY: forwarded from this function's own contract.
        unsafe { restore_termios_and_reraise(sig) };
    }

    /// Register a signal handler for SIGBUS, SIGSEGV, and SIGABRT.
    ///
    /// Flags: `SA_SIGINFO | SA_ONSTACK | SA_RESETHAND`. `SA_RESETHAND`
    /// resets disposition to `SIG_DFL` after delivery, preventing recursive
    /// faults in the handler from looping. The handlers additionally restore
    /// `SIG_DFL` and re-raise explicitly, so the process still terminates
    /// with the original signal's semantics (exit status, core dumps).
    ///
    /// # Safety
    ///
    /// `handler` must be a valid `sa_sigaction`-compatible function pointer.
    unsafe fn register_crash_signals(
        handler: unsafe extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void),
    ) {
        // SAFETY: `sa` is zero-initialised (a valid `sigaction`) and then
        // fully populated; the caller guarantees `handler` has the
        // `SA_SIGINFO` signature.
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = handler as *const () as usize;
            sa.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK | libc::SA_RESETHAND;
            libc::sigemptyset(&mut sa.sa_mask);

            libc::sigaction(libc::SIGBUS, &sa, std::ptr::null_mut());
            libc::sigaction(libc::SIGSEGV, &sa, std::ptr::null_mut());
            libc::sigaction(libc::SIGABRT, &sa, std::ptr::null_mut());
        }
    }

    /// Minimal handler: restore termios only (no escape codes), then re-raise.
    unsafe extern "C" fn terminal_restore_handler_basic(
        sig: libc::c_int,
        _info: *mut libc::siginfo_t,
        _ctx: *mut libc::c_void,
    ) {
        // SAFETY: only ever invoked by the kernel as a signal handler.
        unsafe { restore_termios_and_reraise(sig) };
    }

    /// Minimal handler: restore escape codes + termios, then re-raise.
    unsafe extern "C" fn terminal_restore_handler(
        sig: libc::c_int,
        _info: *mut libc::siginfo_t,
        _ctx: *mut libc::c_void,
    ) {
        // SAFETY: only ever invoked by the kernel as a signal handler.
        unsafe { restore_terminal_and_reraise(sig) };
    }

    /// Install a minimal SIGSEGV/SIGBUS/SIGABRT handler that restores termios
    /// on crash.
    ///
    /// Does NOT write terminal escape codes — call
    /// [`enable_terminal_escape_restore`] after TUI modes are enabled.
    pub fn install_terminal_restore_only() {
        save_termios();
        setup_alt_stack();
        // SAFETY: `terminal_restore_handler_basic` has the SA_SIGINFO signature.
        unsafe { register_crash_signals(terminal_restore_handler_basic) };
    }

    /// Upgrade SIGSEGV/SIGBUS/SIGABRT handlers to include terminal escape
    /// code restoration. Call when TUI modes are enabled.
    pub fn enable_terminal_escape_restore() {
        // SAFETY: `terminal_restore_handler` has the SA_SIGINFO signature.
        unsafe { register_crash_signals(terminal_restore_handler) };
    }

    /// Downgrade SIGSEGV/SIGBUS/SIGABRT handlers to termios-only restoration.
    /// Call when TUI modes are disabled.
    pub fn disable_terminal_escape_restore() {
        // SAFETY: `terminal_restore_handler_basic` has the SA_SIGINFO signature.
        unsafe { register_crash_signals(terminal_restore_handler_basic) };
    }
}

#[cfg(unix)]
pub use imp::{
    disable_terminal_escape_restore, enable_terminal_escape_restore, install_terminal_restore_only,
};

#[cfg(not(unix))]
pub fn install_terminal_restore_only() {}

#[cfg(not(unix))]
pub fn enable_terminal_escape_restore() {}

#[cfg(not(unix))]
pub fn disable_terminal_escape_restore() {}

#[cfg(all(test, unix))]
mod tests {
    use std::sync::Mutex;

    // SIGSEGV/SIGBUS/SIGABRT handlers are process-global. Tests in this binary
    // run on parallel threads, so any two tests that install/read these
    // handlers race. Serialize them through this lock (poison-tolerant: a real
    // assertion failure in one test must not cascade into the other).
    static SIGNAL_STATE_LOCK: Mutex<()> = Mutex::new(());

    /// Query the current disposition of `sig`.
    fn current_action(sig: libc::c_int) -> libc::sigaction {
        // SAFETY: a null `act` makes `sigaction` a pure query into `sa`.
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            assert_eq!(libc::sigaction(sig, std::ptr::null(), &mut sa), 0);
            sa
        }
    }

    #[test]
    fn install_terminal_restore_only_registers_handlers() {
        let _guard = SIGNAL_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        super::install_terminal_restore_only();
        // Note: SA_RESETHAND is set in our sigaction call but macOS XNU does
        // not round-trip it through the sigaction query. The flag IS honored
        // for delivery; the integration tests that assert the process dies
        // with the original signal rely on it.
        for (sig, name) in [
            (libc::SIGSEGV, "SIGSEGV"),
            (libc::SIGBUS, "SIGBUS"),
            (libc::SIGABRT, "SIGABRT"),
        ] {
            let sa = current_action(sig);
            assert_ne!(
                sa.sa_sigaction,
                libc::SIG_DFL,
                "{name} handler should not be SIG_DFL after install"
            );
            assert_ne!(
                sa.sa_flags & libc::SA_ONSTACK,
                0,
                "{name} handler must use alternate signal stack"
            );
        }
    }

    #[test]
    fn escape_restore_toggles_the_installed_handler() {
        let _guard = SIGNAL_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        super::install_terminal_restore_only();
        let basic = current_action(libc::SIGSEGV).sa_sigaction;

        super::enable_terminal_escape_restore();
        let armed = current_action(libc::SIGSEGV).sa_sigaction;
        assert_ne!(
            armed, basic,
            "enable should swap in the escape-code handler"
        );

        super::disable_terminal_escape_restore();
        assert_eq!(
            current_action(libc::SIGSEGV).sa_sigaction,
            basic,
            "disable should restore the termios-only handler"
        );
    }
}
