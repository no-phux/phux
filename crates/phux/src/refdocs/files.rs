//! The generated file-locations reference: the symbolic path rule for
//! every file phux reads or writes.
//!
//! Every rule is stated symbolically (`$XDG_STATE_HOME/phux/server.log`),
//! never as an expanded host path — the generator must be byte-idempotent
//! across machines, and a page carrying `/Users/<someone>/...` would
//! churn on every contributor's checkout (and leak their home layout).
//! Two test layers keep the page honest:
//!
//! - `page_carries_no_expanded_host_paths` scans the render for `/Users/`
//!   and `/home/` substrings;
//! - `path_rules_pin_the_resolving_functions` asserts each documented
//!   trailing shape against the actual resolver — `server_log_path()`,
//!   `default_client_log_path()`, `config_path()`, `default_cert_path()`,
//!   `default_key_path()`, `default_token_store_path()`,
//!   `default_socket_path()` — so a resolver that moves a file forces
//!   this page to move with it (via the freshness test in `super`).

use super::Page;

/// Render `docs/reference/files.md`.
pub(crate) fn page() -> Page {
    let body = String::from(
        "phux splits its files across the three XDG base directories: \
         config (hand-written, follows you between machines), runtime \
         (expected to disappear on reboot), and state (survives across \
         runs but is not config). Paths below are symbolic; each rule \
         names its fallback when the environment variable is unset.\n\n\
         ## Socket\n\n\
         The Unix domain socket every consumer dials and the server \
         binds:\n\n\
         1. `$PHUX_SOCKET` if set (an explicit `--socket` flag still \
            overrides it);\n\
         2. `$XDG_RUNTIME_DIR/phux/phux.sock` if `XDG_RUNTIME_DIR` is \
            set;\n\
         3. `/tmp/phux-<user>/phux.sock` otherwise.\n\n\
         The parent directory is created mode `0700`.\n\n\
         ## Config\n\n\
         `$XDG_CONFIG_HOME/phux/config.toml`, falling back to \
         `~/.config/phux/config.toml` when `XDG_CONFIG_HOME` is unset. \
         A missing config file is not an error (the embedded defaults \
         apply); there is no global config-path flag — set \
         `XDG_CONFIG_HOME` to isolate configuration for a test or an \
         alternate environment. `phux config path` prints the resolved \
         path.\n\n\
         ## State\n\n\
         The state directory is `$XDG_STATE_HOME/phux`, falling back to \
         `~/.local/state/phux` when `XDG_STATE_HOME` is unset or \
         empty:\n\n\
         ```\n\
         $XDG_STATE_HOME/phux/\n\
         ├── server.log        # the ONE server log, both spawn paths\n\
         ├── client-<pid>.log  # per-pid interactive-client log\n\
         ├── remote-cert.pem   # auto-provisioned remote-consumer certificate\n\
         ├── remote-key.pem    # its private key (owner-only, 0600)\n\
         └── remote-tokens     # pairing-token store (owner-only, 0600)\n\
         ```\n\n\
         - `server.log` is the canonical server log regardless of how \
           the server was started: the auto-spawn path redirects the \
           daemon's stderr here, and the service unit points its log \
           capture at the same file. `phux logs` and `phux service \
           logs` read it; `PHUX_LOG` tees the server's structured log \
           to an additional file without moving this one.\n\
         - `client-<pid>.log` is where an interactive client writes its \
           trace — the TUI owns the alt screen, so the client never \
           logs to stderr. `PHUX_LOG` redirects it. Log files are \
           created mode `0600`.\n\
         - `remote-cert.pem` / `remote-key.pem` are the self-signed \
           TLS pair auto-provisioned for remote consumers (ADR-0031); \
           `PHUX_WS_TLS_CERT` / `PHUX_WS_TLS_KEY` substitute an \
           operator-supplied pair. A complete pair is never \
           regenerated, so the pinned fingerprint stays stable.\n\
         - `remote-tokens` is the pairing-token store the server reads \
           and `phux pair` appends to; `PHUX_WS_TOKENS` moves it.\n\n\
         ## Design intent (not yet implemented)\n\n\
         A `server.pid` file and a `journal/` directory of per-pane PTY \
         output for crash recovery remain design intent; neither path \
         exists today. Workspace archives are written only where `phux \
         workspace save` is pointed.\n",
    );

    Page {
        file: "files.md",
        title: "phux file locations reference",
        summary: "The symbolic path rule for every file phux reads or \
                  writes: socket, config, logs, TLS material, tokens.",
        tldr: "Where phux keeps its files: the socket under \
               `$XDG_RUNTIME_DIR` (or `/tmp`), config under \
               `$XDG_CONFIG_HOME`, and logs, TLS material, and pairing \
               tokens under `$XDG_STATE_HOME/phux`. Each rule is pinned \
               to the resolving function by a unit test, so the page \
               moves when the code does.",
        body,
    }
}

#[cfg(test)]
mod tests {
    use super::page;

    /// The page must stay symbolic: an expanded host path would make the
    /// generator non-idempotent across machines and leak a contributor's
    /// home layout into the tree.
    #[test]
    fn page_carries_no_expanded_host_paths() {
        let rendered = page().render();
        for needle in ["/Users/", "/home/"] {
            assert!(
                !rendered.contains(needle),
                "files.md must never carry an expanded host path ({needle})"
            );
        }
    }

    /// Each documented rule is pinned to the function that resolves it:
    /// the resolver's output must end with the documented shape (the
    /// prefix varies with the environment; the shape must not).
    #[test]
    fn path_rules_pin_the_resolving_functions() {
        let ends_with = |path: &std::path::Path, suffix: &str| {
            let rendered = path.display().to_string();
            assert!(
                rendered.ends_with(suffix),
                "documented rule drifted: {rendered} does not end with {suffix}"
            );
        };

        ends_with(
            &phux_server::telemetry::server_log_path(),
            "phux/server.log",
        );
        ends_with(
            &phux_server::telemetry::default_client_log_path(),
            &format!("phux/client-{}.log", std::process::id()),
        );
        ends_with(&phux_config::loader::config_path(), "phux/config.toml");
        ends_with(
            &phux_server::transport::tls::default_cert_path(),
            "phux/remote-cert.pem",
        );
        ends_with(
            &phux_server::transport::tls::default_key_path(),
            "phux/remote-key.pem",
        );
        ends_with(
            &phux_server::auth::default_token_store_path(),
            "phux/remote-tokens",
        );
        // `$PHUX_SOCKET` overrides the whole path, so the shape holds
        // only when the variable is unset (as it is in the test runner;
        // guarded so an operator's environment cannot fail the suite).
        if std::env::var_os("PHUX_SOCKET").is_none() {
            ends_with(&phux_config::socket::default_socket_path(), "/phux.sock");
        }
    }

    /// The documented state-dir entries and the section list stay in
    /// sync with the page prose (spot pins for the tree block).
    #[test]
    fn page_names_every_state_dir_entry() {
        let body = &page().body;
        for entry in [
            "server.log",
            "client-<pid>.log",
            "remote-cert.pem",
            "remote-key.pem",
            "remote-tokens",
            "server.pid",
            "journal/",
        ] {
            assert!(body.contains(entry), "files.md no longer names `{entry}`");
        }
    }
}
