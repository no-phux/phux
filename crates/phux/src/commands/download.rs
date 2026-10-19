//! Thin headless download consumer: `GET_FILE` chunks into a local file.
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use phux_client::download::{DownloadOutcome, DownloadReceipt, DownloadRequest};

use super::json_err::{self, CliError, codes};
use super::server_target::ServerSpec;

pub(crate) struct DownloadArgs {
    pub source: String,
    pub local: Option<PathBuf>,
    pub force: bool,
    pub timeout: u64,
    pub json: bool,
}

pub(crate) fn run(args: DownloadArgs, spec: ServerSpec) -> ExitCode {
    let start = Instant::now();
    let DownloadArgs {
        source,
        local,
        force,
        timeout,
        json,
    } = args;
    if !(1..=3600).contains(&timeout) {
        return fail(json, "timeout must be 1..=3600 seconds", 2);
    }
    let (host, remote_path) = split_source(&source);
    let remote = Path::new(remote_path);
    let Some(file_name) = remote.file_name().filter(|_| remote.is_absolute()) else {
        return fail(
            json,
            "SOURCE must be an absolute path to a file on the host, as PATH or HOST:PATH",
            2,
        );
    };
    let mut local_path = local.unwrap_or_else(|| PathBuf::from(file_name));
    if local_path.is_dir() {
        local_path.push(file_name);
    }
    // The authoritative guard is the exclusive publish in the client helper;
    // this only saves a connection when the answer is already known.
    if local_path.exists() && !force {
        return fail(
            json,
            &format!(
                "{} exists; pass --force to replace it",
                local_path.display()
            ),
            2,
        );
    }
    let request = DownloadRequest {
        host: host.map(str::to_owned),
        remote_path: remote_path.to_owned(),
        local_path,
        force,
    };
    // Remote planning can block in DNS. Download resolution never pairs.
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    if let Err(err) = std::thread::Builder::new()
        .name("download-resolve".into())
        .spawn(move || {
            let _ = sender.send(spec.prepare("download", json));
        })
    {
        return fail(
            json,
            &format!("could not start target resolution: {err}"),
            1,
        );
    }
    let (rt, server) =
        match receiver.recv_timeout(Duration::from_secs(timeout).saturating_sub(start.elapsed())) {
            Ok(Ok(value)) => value,
            Ok(Err(code)) => return code,
            Err(_) => {
                return fail(
                    json,
                    "download deadline elapsed during server resolution; nothing written",
                    1,
                );
            }
        };
    let Some(budget) = Duration::from_secs(timeout).checked_sub(start.elapsed()) else {
        return fail(
            json,
            "download deadline elapsed before target resolution",
            1,
        );
    };
    let status = rt.block_on(async {
        let deadline = tokio::time::Instant::now() + budget;
        let mut conn = match server.connect().await {
            Ok(conn) => conn,
            Err(err) => return server.report_unreachable(json, &err, "download"),
        };
        match phux_client::download::download_on(&mut conn, request, deadline).await {
            Ok(receipt) => print_receipt(&receipt, json),
            Err(err) => fail(json, &err.to_string(), 1),
        }
    });
    rt.shutdown_background();
    status
}

/// `HOST:/abs/path` names a satellite of the connected server; a bare
/// absolute path reads the server itself. The split is only taken when the
/// prefix contains no `/`, so an absolute path is never mistaken for a host.
fn split_source(source: &str) -> (Option<&str>, &str) {
    match source.split_once(':') {
        Some((host, path)) if !host.is_empty() && !host.contains('/') && path.starts_with('/') => {
            (Some(host), path)
        }
        _ => (None, source),
    }
}

fn print_receipt(receipt: &DownloadReceipt, json: bool) -> ExitCode {
    let completed = receipt.outcome == DownloadOutcome::Completed;
    if json {
        outln!(
            "{}",
            serde_json::json!({"schema_version": 1, "receipt": receipt})
        );
    } else if completed {
        outln!("{}", receipt.local_path.display());
    }
    if completed {
        ExitCode::SUCCESS
    } else {
        warn!("download {:?}: {}", receipt.outcome, receipt.message);
        ExitCode::FAILURE
    }
}

fn fail(json: bool, message: &str, status: u8) -> ExitCode {
    json_err::emit(
        json,
        &CliError::new(
            codes::TRANSPORT,
            message,
            "check the source path, the local destination, and the server",
        ),
        status,
    )
}

#[cfg(test)]
mod tests {
    use super::split_source;

    #[test]
    fn a_host_prefix_is_taken_only_before_an_absolute_path() {
        assert_eq!(split_source("/a/b"), (None, "/a/b"));
        assert_eq!(split_source("mini:/a/b"), (Some("mini"), "/a/b"));
        assert_eq!(split_source("mini:relative"), (None, "mini:relative"));
        assert_eq!(split_source("/odd:dir/f"), (None, "/odd:dir/f"));
        assert_eq!(split_source(":/a"), (None, ":/a"));
    }
}
