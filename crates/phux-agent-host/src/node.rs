//! Own-code packet transport only. Provider execution stays inside the official
//! Node SDKs loaded by the bridge from the parent-supplied isolated runtime.
use crate::{HostError, HostSpec, Sink, common};
use phux_protocol::native_agent::*;
use serde::Deserialize;
use std::{
    io::{BufRead, BufReader, Read, Write},
    process::Command,
    thread,
    time::Duration,
};
use tokio::sync::{mpsc, watch};

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Packet {
    Ready {
        provider_session_id: Option<String>,
        capabilities: NativeCapabilities,
    },
    Update {
        update: NativeUpdate,
    },
    Result {
        result: NodeResult,
    },
    Failed {
        message: String,
    },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NodeResult {
    operation_id: String,
    disposition: NativeDisposition,
    detail: Option<String>,
}
impl NodeResult {
    fn into_native(self) -> Result<NativeCommandResult, HostError> {
        let operation_id = self
            .operation_id
            .parse::<u64>()
            .map_err(|_| HostError::Sdk("bridge operation id is not u64 decimal".into()))?;
        if operation_id.to_string() != self.operation_id {
            return Err(HostError::Sdk(
                "bridge operation id is not canonical decimal".into(),
            ));
        }
        Ok(NativeCommandResult {
            operation_id,
            disposition: self.disposition,
            detail: self.detail,
        })
    }
}
pub(super) fn run(
    spec: HostSpec,
    sink: Sink,
    mut commands: mpsc::Receiver<NativeCommand>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), HostError> {
    let bridge = spec
        .node_bridge
        .as_ref()
        .ok_or_else(|| HostError::Invalid("node_bridge is required".into()))?;
    let runtime_path = spec
        .sdk_runtime
        .as_ref()
        .ok_or_else(|| HostError::Invalid("isolated sdk_runtime is required".into()))?;
    let executable = spec.env.get("PHUX_NATIVE_NODE_EXECUTABLE").ok_or_else(|| {
        HostError::Invalid("parent must supply PHUX_NATIVE_NODE_EXECUTABLE".into())
    })?;
    if !std::path::Path::new(executable).is_absolute() {
        return Err(HostError::Invalid(
            "Node executable must be an absolute runtime-owned path".into(),
        ));
    }
    let mut command = Command::new(executable);
    command.arg(bridge);
    common::configure(&mut command, &spec);
    command
        .env("PHUX_NATIVE_PROVIDER", &spec.provider)
        .env(
            "PHUX_NATIVE_COMMAND",
            serde_json::to_string(&spec.command)
                .map_err(|error| HostError::Invalid(error.to_string()))?,
        )
        .env("PHUX_NATIVE_ACTIVATION", &spec.activation)
        .env("PHUX_NATIVE_CWD", &spec.cwd)
        .env("PHUX_NATIVE_SDK_RUNTIME", runtime_path);
    if let Some(id) = &spec.native_id {
        command.env("PHUX_NATIVE_SESSION_ID", id);
    }
    let mut child = common::ChildGuard(command.spawn()?);
    let stdout = child
        .0
        .stdout
        .take()
        .ok_or_else(|| HostError::Sdk("missing bridge stdout".into()))?;
    let mut stdin = child
        .0
        .stdin
        .take()
        .ok_or_else(|| HostError::Sdk("missing bridge stdin".into()))?;
    let output_sink = sink.clone();
    let reader = thread::spawn(move || -> Result<(), HostError> {
        let mut reader = BufReader::new(stdout);
        let mut bytes = Vec::with_capacity(16 * 1024);
        loop {
            bytes.clear();
            let count = Read::by_ref(&mut reader)
                .take((MAX_NATIVE_PAYLOAD_BYTES + 1) as u64)
                .read_until(b'\n', &mut bytes)?;
            if count == 0 {
                return Ok(());
            }
            if bytes.len() > MAX_NATIVE_PAYLOAD_BYTES || bytes.last() != Some(&b'\n') {
                return Err(HostError::Sdk(
                    "bridge packet exceeds 16 KiB or has incomplete framing".into(),
                ));
            }
            let packet: Packet = serde_json::from_slice(&bytes).map_err(|error| {
                HostError::Sdk(format!("bridge stdout is not typed protocol: {error}"))
            })?;
            match packet {
                Packet::Ready {
                    provider_session_id,
                    capabilities,
                } => {
                    let mut view = output_sink.view();
                    view.connected = true;
                    view.capabilities = capabilities;
                    if provider_session_id.is_some() {
                        view.provider_session_id = provider_session_id;
                    }
                    output_sink.update(NativeUpdate::View { view })?;
                }
                Packet::Update { update } => output_sink.update(update)?,
                Packet::Result { result } => output_sink.result(result.into_native()?)?,
                Packet::Failed { message } => return Err(HostError::Sdk(message)),
            }
        }
    });
    let (send, receive) = std::sync::mpsc::sync_channel::<NativeCommand>(MAX_NATIVE_PENDING);
    let stopping = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writer_stopping = stopping.clone();
    let writer = thread::spawn(move || -> Result<(), HostError> {
        while let Ok(command) = receive.recv() {
            if writer_stopping.load(std::sync::atomic::Ordering::Acquire) {
                break;
            }
            let packet = serde_json::json!({"type":"command", "operation_id":command.operation_id.to_string(), "activation":command.activation,"intent":command.intent});
            serde_json::to_writer(&mut stdin, &packet)
                .map_err(|error| HostError::Sdk(error.to_string()))?;
            stdin.write_all(b"\n")?;
            stdin.flush()?;
        }
        if let Err(error) = stdin
            .write_all(b"{\"type\":\"shutdown\"}\n")
            .and_then(|()| stdin.flush())
        {
            if error.kind() != std::io::ErrorKind::BrokenPipe {
                return Err(error.into());
            }
        }
        Ok(())
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let outcome = runtime.block_on(async {
        loop {
            tokio::select! {
                _ = shutdown.changed() => return Ok::<(), HostError>(()),
                command = commands.recv() => {
                    let Some(command) = command else { return Ok(()); };
                    send.try_send(command).map_err(|_| HostError::Sdk("bridge command writer unavailable or bounded queue full".into()))?;
                }
                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    if reader.is_finished() || writer.is_finished() || child.0.try_wait()?.is_some() { return Ok(()); }
                }
            }
        }
    });
    stopping.store(true, std::sync::atomic::Ordering::Release);
    drop(send);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while child.0.try_wait()?.is_none() && std::time::Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    drop(child);
    let read = common::join_reader(reader, "bridge reader")?;
    let write = common::join_reader(writer, "bridge writer")?;
    outcome.and(read).and(write)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bridge_results_preserve_queued_semantics() {
        let packet: Packet = serde_json::from_str(r#"{"type":"result","result":{"operation_id":"18446744073709551615","disposition":"queued","detail":"SDK writer"}}"#).unwrap();
        assert!(
            matches!(packet, Packet::Result { result } if result.disposition == NativeDisposition::Queued)
        );
        let result = NodeResult {
            operation_id: u64::MAX.to_string(),
            disposition: NativeDisposition::Queued,
            detail: None,
        }
        .into_native()
        .unwrap();
        assert_eq!(result.operation_id, u64::MAX);
        assert!(serde_json::from_str::<Packet>("provider diagnostics").is_err());
    }
}
