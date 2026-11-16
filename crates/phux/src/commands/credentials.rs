//! A user-started 1Password provider, and local-only secret environment injection.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, ExitStatus, Stdio};
use std::time::Duration;

use phux_client::attach::connection::Connection;
use phux_client::credentials::{self, CredentialRequest, SecretValues};
use tokio::io::AsyncReadExt;

use super::{CredentialsAction, CredentialsArgs, server_target::ServerSpec};

const PROVIDER_TIMEOUT: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(500);

pub(crate) fn run(args: CredentialsArgs, socket: Option<PathBuf>) -> ExitCode {
    match args.action {
        CredentialsAction::Serve {
            name,
            account,
            remote,
        } => run_serve(&name, account.as_deref(), remote.with_socket(socket)),
        CredentialsAction::Run {
            provider,
            env,
            purpose,
            remote,
            command,
        } => run_local(
            provider,
            &env,
            purpose,
            &command,
            remote.with_socket(socket),
        ),
    }
}

fn run_serve(name: &str, account: Option<&str>, spec: ServerSpec) -> ExitCode {
    if let Err(err) = phux_protocol::credentials::validate_provider(name) {
        return fail(&err);
    }
    let (rt, server) = match spec.prepare("credentials serve", false) {
        Ok(prepared) => prepared,
        Err(code) => return code,
    };
    let result = rt.block_on(async {
        let mut conn = server.connect().await.map_err(|err| err.to_string())?;
        // Cancellation drops any in-flight `op` child (kill_on_drop) and closes
        // the claim's connection, so the broker fails the waiting requester.
        let result = tokio::select! {
            result = serve(&mut conn, name, account) => result,
            signal = tokio::signal::ctrl_c() => signal.map_err(|_| "could not observe interrupt".to_owned()),
        };
        conn.shutdown().await;
        result
    });
    result.map_or_else(|err| fail(&err), |()| ExitCode::SUCCESS)
}

async fn serve(conn: &mut Connection, name: &str, account: Option<&str>) -> Result<(), String> {
    let mut announced = false;
    loop {
        let job = credentials::next(conn, name)
            .await
            .map_err(|err| err.to_string())?;
        if !announced {
            warn!(
                "credentials: provider {name} connected; waiting for approved 1Password requests"
            );
            announced = true;
        }
        if let Some(job) = job {
            fulfill(conn, job, account).await?;
        } else {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

async fn fulfill(
    conn: &mut Connection,
    job: credentials::ProviderJob,
    account: Option<&str>,
) -> Result<(), String> {
    let values = resolve(&job.request, account, Path::new("op")).await;
    if values.is_err() {
        warn!("credentials: 1Password could not fulfill an approved request; no values returned");
    }
    match credentials::complete(conn, job.id, values.ok()).await {
        Ok(()) => {}
        Err(credentials::CredentialError::Refused {
            code: phux_protocol::wire::frame::ErrorCode::PermissionDenied,
            ..
        }) => {
            // Cancellation/expiry spends just this claim. Do not replay it
            // or disconnect other approved exchanges. If our grant was
            // revoked, the next poll will fail and retire this provider.
            warn!("credentials: approved request ended before delivery; not retried");
        }
        Err(error) => return Err(error.to_string()),
    }
    Ok(())
}

async fn resolve(
    request: &CredentialRequest,
    account: Option<&str>,
    executable: &Path,
) -> Result<SecretValues, &'static str> {
    request
        .validate()
        .map_err(|_| "invalid credential request")?;
    tokio::time::timeout(
        PROVIDER_TIMEOUT,
        read_references(request, account, executable),
    )
    .await
    .map_err(|_| "1Password read timed out")?
}

async fn read_references(
    request: &CredentialRequest,
    account: Option<&str>,
    executable: &Path,
) -> Result<SecretValues, &'static str> {
    let mut values = SecretValues(Vec::with_capacity(request.references.len()));
    let mut total = 0_usize;
    for reference in &request.references {
        let value = read_secret(executable, account, reference).await?;
        total = total.saturating_add(value.len());
        if total > credentials::MAX_TOTAL_BYTES {
            return Err("1Password response exceeds the total size limit");
        }
        values.0.push(value);
    }
    Ok(values)
}

fn op_command(
    executable: &Path,
    account: Option<&str>,
    reference: &str,
) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(executable);
    command.args(["read", "--no-newline"]);
    if let Some(account) = account {
        command.args(["--account", account]);
    }
    command.args(["--", reference]);
    // Desktop approval is the only authentication source. In particular, never
    // inherit OP_SERVICE_ACCOUNT_TOKEN, OP_SESSION_*, OP_CONNECT_*, or OP_RUN_*.
    command
        .env_clear()
        .envs(std::env::vars_os().filter(|(key, _)| !key.to_string_lossy().starts_with("OP_")));
    command.env("OP_BIOMETRIC_UNLOCK_ENABLED", "true");
    command
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .kill_on_drop(true);
    command
}

async fn read_secret(
    executable: &Path,
    account: Option<&str>,
    reference: &str,
) -> Result<String, &'static str> {
    let mut child = op_command(executable, account, reference)
        .spawn()
        .map_err(|_| "could not start 1Password CLI")?;
    let stdout = child.stdout.take().ok_or("1Password stdout unavailable")?;
    let mut bytes = Vec::new();
    stdout
        .take((credentials::MAX_SECRET_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| "could not read 1Password response")?;
    if bytes.len() > credentials::MAX_SECRET_BYTES {
        return Err("1Password response exceeds the field size limit");
    }
    let status = child
        .wait()
        .await
        .map_err(|_| "could not wait for 1Password CLI")?;
    if !status.success() {
        return Err("1Password read failed");
    }
    let value = String::from_utf8(bytes).map_err(|_| "1Password response is not UTF-8")?;
    if value.contains('\0') {
        return Err("1Password response contains NUL");
    }
    Ok(value)
}

fn run_local(
    provider: String,
    env: &[String],
    purpose: String,
    command: &[OsString],
    spec: ServerSpec,
) -> ExitCode {
    let assignments = match parse_assignments(env) {
        Ok(assignments) => assignments,
        Err(err) => return fail(err),
    };
    let request = CredentialRequest {
        provider,
        references: assignments
            .iter()
            .map(|(_, reference)| reference.clone())
            .collect(),
        purpose,
    };
    if let Err(err) = request.validate() {
        return fail(&err);
    }
    let (rt, server) = match spec.prepare("credentials run", false) {
        Ok(prepared) => prepared,
        Err(code) => return code,
    };
    let values = rt.block_on(async {
        let mut conn = server.connect().await.map_err(|err| err.to_string())?;
        let result = credentials::request(&mut conn, request)
            .await
            .map_err(|err| err.to_string());
        conn.shutdown().await;
        result
    });
    let values = match values {
        Ok(values) => values,
        Err(err) => return fail(&err),
    };
    match injected_command(command, &assignments, values).and_then(|mut child| {
        child
            .status()
            .map_err(|_| "could not execute the local credential consumer")
    }) {
        Ok(status) => exit_status(status),
        Err(err) => fail(err),
    }
}

fn parse_assignments(env: &[String]) -> Result<Vec<(String, String)>, &'static str> {
    let mut assignments = Vec::with_capacity(env.len());
    for assignment in env {
        let (name, reference) = assignment
            .split_once('=')
            .ok_or("--env must be NAME=op://VAULT/ITEM/FIELD")?;
        if !valid_env_name(name) {
            return Err("credential environment names must match [A-Za-z_][A-Za-z0-9_]*");
        }
        if assignments.iter().any(|(existing, _)| existing == name) {
            return Err("credential environment names must be unique");
        }
        assignments.push((name.to_owned(), reference.to_owned()));
    }
    if assignments.is_empty() {
        return Err("at least one --env reference is required");
    }
    Ok(assignments)
}

fn valid_env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return false;
    }
    bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn injected_command(
    argv: &[OsString],
    assignments: &[(String, String)],
    values: SecretValues,
) -> Result<std::process::Command, &'static str> {
    let (program, arguments) = argv
        .split_first()
        .ok_or("a local COMMAND is required after --")?;
    if assignments.len() != values.0.len() {
        return Err("credential broker returned the wrong number of values");
    }
    if values.0.iter().any(|value| value.contains('\0')) {
        return Err("credential broker returned a value that cannot be injected");
    }
    let mut command = std::process::Command::new(program);
    command.args(arguments);
    for ((name, _), value) in assignments.iter().zip(&values.0) {
        command.env(name, value);
    }
    drop(values);
    Ok(command)
}

fn exit_status(status: ExitStatus) -> ExitCode {
    use std::os::unix::process::ExitStatusExt;
    let code = status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(1));
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

fn fail(message: &str) -> ExitCode {
    warn!("credentials: {message}");
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test setup and assertions")]

    use super::*;
    use std::ffi::OsStr;
    use std::os::unix::fs::PermissionsExt;

    fn request(references: Vec<String>) -> CredentialRequest {
        CredentialRequest {
            provider: "test".into(),
            references,
            purpose: "test fixture".into(),
        }
    }

    fn fake_op(body: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("fixture directory");
        let path = dir.path().join("op");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("fixture executable");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .expect("executable permissions");
        (dir, path)
    }

    #[tokio::test]
    async fn fixed_read_arguments_and_desktop_environment() {
        let (_dir, op) = fake_op(
            r#"
            [ "$#" = 4 ] || exit 1
            [ "$1" = read ] && [ "$2" = --no-newline ] && [ "$3" = -- ] || exit 1
            [ "$4" = 'op://vault/item/field name' ] || exit 1
            [ "$OP_BIOMETRIC_UNLOCK_ENABLED" = true ] || exit 1
            printf fixture
        "#,
        );
        let request = request(vec!["op://vault/item/field name".into()]);
        let values = resolve(&request, None, &op)
            .await
            .expect("bounded fake read");
        assert_eq!(values.0, ["fixture"]);
        let command = op_command(Path::new("op"), Some("my-account"), &request.references[0]);
        assert_eq!(
            command.as_std().get_args().collect::<Vec<_>>(),
            [
                "read",
                "--no-newline",
                "--account",
                "my-account",
                "--",
                "op://vault/item/field name"
            ]
            .map(OsStr::new)
        );
        let op_env: Vec<_> = command
            .as_std()
            .get_envs()
            .filter(|(key, _)| key.to_string_lossy().starts_with("OP_"))
            .collect();
        assert_eq!(
            op_env,
            [(
                OsStr::new("OP_BIOMETRIC_UNLOCK_ENABLED"),
                Some(OsStr::new("true"))
            )]
        );
    }

    #[tokio::test]
    async fn provider_failures_never_return_partial_or_unbounded_values() {
        let (_dir, op) = fake_op("printf secret-on-stdout; printf secret-on-stderr >&2; exit 1");
        let input = request(vec!["op://vault/item/field".into()]);
        assert!(resolve(&input, None, &op).await.is_err());
        let (_dir, op) = fake_op("head -c 65537 /dev/zero | tr '\\000' x");
        assert!(resolve(&input, None, &op).await.is_err());
        let (_dir, op) = fake_op("head -c 65536 /dev/zero | tr '\\000' x");
        let input = request(vec!["op://vault/item/field".into(); 3]);
        assert!(resolve(&input, None, &op).await.is_err());
    }

    #[tokio::test]
    async fn malformed_and_partial_provider_output_is_not_delivered() {
        let input = request(vec!["op://vault/item/field".into()]);
        for body in ["printf '\\000'", "printf '\\377'"] {
            let (_dir, op) = fake_op(body);
            assert!(resolve(&input, None, &op).await.is_err());
        }
        let (_dir, op) = fake_op("[ \"$4\" = op://vault/item/first ] || exit 1; printf fixture");
        let input = request(vec![
            "op://vault/item/first".into(),
            "op://vault/item/second".into(),
        ]);
        assert!(resolve(&input, None, &op).await.is_err());
    }

    #[tokio::test]
    async fn provider_reads_are_cancellable_and_use_a_bounded_deadline() {
        assert_eq!(PROVIDER_TIMEOUT, Duration::from_secs(60));
        let (dir, op) = fake_op("printf '%s' \"$$\" > \"$0.pid\"; exec sleep 3600");
        let input = request(vec!["op://vault/item/field".into()]);
        assert!(
            tokio::time::timeout(
                Duration::from_millis(500),
                read_references(&input, None, &op)
            )
            .await
            .is_err()
        );
        let pid = std::fs::read_to_string(dir.path().join("op.pid"))
            .expect("the owned provider child recorded its pid");
        tokio::time::timeout(Duration::from_secs(2), async {
            while std::process::Command::new("/bin/kill")
                .args(["-0", &pid])
                .stderr(Stdio::null())
                .status()
                .expect("probe the recorded provider pid")
                .success()
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("cancelled provider child is killed and reaped");
    }

    #[test]
    fn environment_mapping_rejects_bad_or_duplicate_names() {
        for assignments in [
            vec![],
            vec!["missing".into()],
            vec!["1TOKEN=op://v/i/f".into()],
            vec!["A=op://v/i/f".into(); 2],
        ] {
            assert!(parse_assignments(&assignments).is_err());
        }
        assert!(parse_assignments(&["TOKEN_1=op://vault/item/field".into()]).is_ok());
    }

    #[test]
    fn consumer_receives_only_environment_values_and_preserves_status() {
        let argv = [
            "/bin/sh",
            "-c",
            "test -n \"$PHUX_TEST_CREDENTIAL\" || exit 1; exit 37",
        ]
        .map(OsString::from);
        let env = vec![(
            "PHUX_TEST_CREDENTIAL".into(),
            "op://vault/item/field".into(),
        )];
        let mut command =
            injected_command(&argv, &env, SecretValues(vec!["fixture".into()])).expect("injection");
        assert!(!command.get_args().any(|arg| arg == "fixture"));
        assert_eq!(
            exit_status(command.status().expect("consumer exit")),
            ExitCode::from(37)
        );
        assert!(injected_command(&argv, &env, SecretValues(vec![])).is_err());
        assert!(injected_command(&argv, &env, SecretValues(vec!["a\0b".into()])).is_err());
    }

    #[test]
    fn signaled_consumer_uses_the_shell_exit_status_convention() {
        let status = std::process::Command::new("/bin/sh")
            .args(["-c", "kill -TERM $$"])
            .status()
            .expect("self-signaling consumer");
        assert_eq!(exit_status(status), ExitCode::from(143));
    }
}
