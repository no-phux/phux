//! Reading frames from a network link, and the split reader and writer halves.

use super::{
    FramingError, LinkReader, LinkWriter, NetLinkConn, NetLinkReader, NetLinkWriter,
    encode_frame_too_large, framing_loss_reason, read_length_prefixed, send_bounded,
    ssh_exit_reason, ws_idle_error,
};

impl LinkReader for NetLinkConn {
    async fn recv_frame(&mut self) -> Result<Option<Vec<u8>>, String> {
        // A framing violation is carried out of the match (whose arms
        // borrow `self`) so the goodbye has one send path for every
        // transport.
        let violation = 'framing: {
            match self {
                Self::Quic { recv, buf, .. } => {
                    // Cancel-safe: bytes land in the persistent buffer.
                    loop {
                        match phux_protocol::wire::framing::split_frame(buf) {
                            Ok(Some(framed)) => return Ok(Some(framed.to_vec())),
                            Ok(None) => {}
                            Err(violation) => break 'framing violation,
                        }
                        let n = tokio::io::AsyncReadExt::read_buf(recv, buf)
                            .await
                            .map_err(|err| format!("read from satellite: {err}"))?;
                        if n == 0 {
                            if buf.is_empty() {
                                return Ok(None);
                            }
                            return Err("satellite closed the stream mid-frame".to_owned());
                        }
                    }
                }
                Self::Ws {
                    ws, last_inbound, ..
                } => loop {
                    match futures_util::StreamExt::next(ws.as_mut()).await {
                        None => return Ok(None),
                        Some(Ok(message)) => {
                            // Any inbound message counts as liveness.
                            *last_inbound = std::time::Instant::now();
                            match message {
                                tokio_tungstenite::tungstenite::Message::Close(_) => {
                                    return Ok(None);
                                }
                                tokio_tungstenite::tungstenite::Message::Binary(data) => {
                                    // One binary message is one frame (SPEC §5).
                                    if let Err(violation) =
                                        phux_protocol::wire::framing::check_frame(&data)
                                    {
                                        break 'framing violation;
                                    }
                                    return Ok(Some(data.to_vec()));
                                }
                                // Ping/pong: reading answers pings.
                                _ => {}
                            }
                        }
                        Some(Err(err)) => return Err(format!("connection error: {err}")),
                    }
                },
                Self::Ssh {
                    child,
                    stdout,
                    stderr_reader,
                    buf,
                    ..
                } => {
                    // Same reassembly as QUIC. stdout EOF means the link is
                    // gone; the exit status and stderr tail are the reason.
                    loop {
                        match phux_protocol::wire::framing::split_frame(buf) {
                            Ok(Some(framed)) => return Ok(Some(framed.to_vec())),
                            Ok(None) => {}
                            Err(violation) => break 'framing violation,
                        }
                        let n = tokio::io::AsyncReadExt::read_buf(stdout, buf)
                            .await
                            .map_err(|err| format!("read from ssh transport: {err}"))?;
                        if n == 0 {
                            let mut reason = ssh_exit_reason(child, stderr_reader).await;
                            if !buf.is_empty() {
                                reason = format!("{reason} (stream ended mid-frame)");
                            }
                            return Err(reason);
                        }
                    }
                }
            }
        };
        // Best-effort SPEC §5 goodbye; never masks the real loss reason.
        let _ = send_bounded(self, &encode_frame_too_large(violation)).await;
        Err(framing_loss_reason(violation))
    }
}

impl LinkWriter for NetLinkWriter {
    async fn send_frame(&mut self, frame: &[u8]) -> Result<(), String> {
        match self {
            Self::Quic { send, .. } => tokio::io::AsyncWriteExt::write_all(send, frame)
                .await
                .map_err(|err| format!("write to satellite: {err}")),
            Self::Ws { sink, .. } => futures_util::SinkExt::send(
                sink,
                tokio_tungstenite::tungstenite::Message::Binary(frame.to_vec().into()),
            )
            .await
            .map_err(|err| format!("write to satellite: {err}")),
            Self::Ssh { stdin } => tokio::io::AsyncWriteExt::write_all(stdin, frame)
                .await
                .map_err(|err| format!("write to ssh transport: {err}")),
        }
    }

    async fn keepalive(&mut self) -> Result<(), String> {
        match self {
            Self::Quic { .. } | Self::Ssh { .. } => Ok(()),
            Self::Ws {
                sink, last_inbound, ..
            } => {
                let idle_for = last_inbound
                    .lock()
                    .map_err(|_| "satellite liveness clock poisoned".to_owned())?
                    .elapsed();
                if let Some(reason) = ws_idle_error(idle_for) {
                    return Err(reason);
                }
                futures_util::SinkExt::send(
                    sink,
                    tokio_tungstenite::tungstenite::Message::Ping(Vec::new().into()),
                )
                .await
                .map_err(|err| format!("keepalive ping to satellite: {err}"))
            }
        }
    }
}

impl LinkReader for NetLinkReader {
    async fn recv_frame(&mut self) -> Result<Option<Vec<u8>>, String> {
        match self {
            Self::Quic {
                recv,
                buf,
                framing_violation,
                ..
            } => read_length_prefixed(recv, buf, framing_violation, "satellite").await,
            Self::Ws {
                stream,
                last_inbound,
                framing_violation,
            } => loop {
                match futures_util::StreamExt::next(stream).await {
                    None => return Ok(None),
                    Some(Ok(message)) => {
                        *last_inbound
                            .lock()
                            .map_err(|_| "satellite liveness clock poisoned".to_owned())? =
                            std::time::Instant::now();
                        match message {
                            tokio_tungstenite::tungstenite::Message::Close(_) => return Ok(None),
                            tokio_tungstenite::tungstenite::Message::Binary(data) => {
                                if let Err(violation) =
                                    phux_protocol::wire::framing::check_frame(&data)
                                {
                                    *framing_violation = Some(violation);
                                    return Err(framing_loss_reason(violation));
                                }
                                return Ok(Some(data.to_vec()));
                            }
                            _ => {}
                        }
                    }
                    Some(Err(err)) => return Err(format!("connection error: {err}")),
                }
            },
            Self::Ssh {
                child,
                stdout,
                stderr_reader,
                buf,
                framing_violation,
            } => {
                match read_length_prefixed(stdout, buf, framing_violation, "ssh transport").await {
                    Ok(None) => Err(ssh_exit_reason(child, stderr_reader).await),
                    other => other,
                }
            }
        }
    }

    fn take_framing_violation(&mut self) -> Option<FramingError> {
        match self {
            Self::Quic {
                framing_violation, ..
            }
            | Self::Ws {
                framing_violation, ..
            }
            | Self::Ssh {
                framing_violation, ..
            } => framing_violation.take(),
        }
    }
}
