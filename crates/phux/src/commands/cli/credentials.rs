//! Native 1Password provider and local environment injection.

use usage::{Args, Subcommands};

use super::RemoteOpt;

/// Use 1Password credentials with one-time approval
///
/// Start `serve` on the machine with the 1Password desktop app. `run` starts
/// a LOCAL process only after approval and provider completion. Review requests
/// with `phux approvals`; decide them with `phux approve ID` or `phux deny ID`.
#[derive(Debug, Args)]
pub(crate) struct CredentialsArgs {
    #[usage(subcommand)]
    pub(crate) action: CredentialsAction,
}

#[derive(Debug, Subcommands)]
pub(crate) enum CredentialsAction {
    /// Serve approved requests using the local 1Password desktop integration.
    Serve {
        /// Provider name requesters select.
        #[usage(long, value_name = "NAME")]
        name: String,
        /// 1Password account selector (not a secret).
        #[usage(long, value_name = "ACCOUNT")]
        account: Option<String>,
        #[usage(flatten)]
        remote: RemoteOpt,
    },
    /// Execute a LOCAL command with approved secrets injected into its environment.
    Run {
        /// Provider name on the connected broker.
        #[usage(long, value_name = "NAME")]
        provider: String,
        /// Environment assignment to a reference; repeat for multiple variables.
        #[usage(long = "env", value_name = "NAME=op://VAULT/ITEM/FIELD", required)]
        env: Vec<String>,
        /// Human-readable reason shown in the approval request.
        #[usage(long, value_name = "TEXT")]
        purpose: String,
        #[usage(flatten)]
        remote: RemoteOpt,
        /// Local executable and arguments, after --. Never use a secret as an argument.
        #[usage(value_name = "COMMAND", trailing_var_arg, required)]
        command: Vec<std::ffi::OsString>,
    },
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "parse assertions")]

    use super::*;

    #[test]
    fn provider_and_consumer_accept_remote_options_without_consuming_child_flags() {
        let cli = crate::parse_cli([
            "phux",
            "credentials",
            "serve",
            "--name",
            "desktop",
            "--account",
            "personal",
            "--remote",
            "mini",
        ])
        .expect("provider CLI");
        assert_eq!(
            crate::commands::verb_remote(cli.command.as_ref().expect("command")),
            Some("mini")
        );
        let cli = crate::parse_cli([
            "phux",
            "credentials",
            "run",
            "--provider",
            "desktop",
            "--env",
            "TOKEN=op://v/i/f",
            "--purpose",
            "test consumer",
            "--remote",
            "mini",
            "--",
            "consumer",
            "--version",
        ])
        .expect("consumer CLI");
        assert!(
            matches!(cli.command, Some(crate::commands::Command::Credentials(CredentialsArgs {
            action: CredentialsAction::Run { command, .. },
        })) if command == ["consumer", "--version"].map(std::ffi::OsString::from))
        );
    }

    #[test]
    fn consumer_requires_provider_references_purpose_and_command() {
        for args in [
            vec!["phux", "credentials", "serve"],
            vec![
                "phux",
                "credentials",
                "run",
                "--provider",
                "desktop",
                "--purpose",
                "test",
                "--",
                "consumer",
            ],
            vec![
                "phux",
                "credentials",
                "run",
                "--provider",
                "desktop",
                "--env",
                "TOKEN=op://v/i/f",
                "--purpose",
                "test",
            ],
        ] {
            assert!(crate::parse_cli(args).is_err());
        }
    }
}
