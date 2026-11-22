//! Read and write `Global` L3 metadata keys: the last-writer-wins facts a
//! host publishes for every client (`phux.usage/v1`, say). Generic over the
//! key; callers own its meaning.

use phux_protocol::wire::frame::{FrameKind, Scope};

use crate::attach::AttachError;
use crate::attach::connection::{Connection, Refusal};

/// Why a write did not land.
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    /// The connection failed.
    #[error(transparent)]
    Attach(#[from] AttachError),
    /// The server refused the confirming read (and so, in order, the write).
    #[error("the server refused the write: {0}")]
    Refused(Refusal),
}

/// Read `keys` in one pipelined round trip. A key that is absent, or whose
/// read the server refused, reads as `None`.
///
/// # Errors
///
/// Transport and decode failures.
pub async fn read(
    conn: &mut Connection,
    keys: &[&str],
) -> Result<Vec<Option<Vec<u8>>>, AttachError> {
    let reads = keys
        .iter()
        .map(|key| (Scope::Global, (*key).to_owned()))
        .collect();
    Ok(conn
        .request_metadata_many(1, reads)
        .await?
        .into_result_ignoring_interleaved()
        .into_iter()
        .map(|answer| answer.ok().flatten())
        .collect())
}

/// Set `key` to `value`, then read it back. `SET_METADATA` has no reply;
/// the server handles one connection's frames in order, so the read-back is
/// both the flush barrier and where a refusal surfaces.
///
/// # Errors
///
/// [`WriteError::Attach`] on transport failure, [`WriteError::Refused`]
/// when the server refused the read-back.
pub async fn write(conn: &mut Connection, key: &str, value: Vec<u8>) -> Result<(), WriteError> {
    conn.send(&FrameKind::SetMetadata {
        request_id: 1,
        scope: Scope::Global,
        key: key.to_owned(),
        value,
    })
    .await?;
    conn.request_metadata(2, Scope::Global, key.to_owned())
        .await?
        .into_result_ignoring_interleaved()
        .map(drop)
        .map_err(WriteError::Refused)
}

#[cfg(test)]
mod tests {
    use phux_protocol::wire::frame::ErrorCode;

    use super::*;
    use crate::testkit::{ScriptSpec, ScriptedServer};

    fn scripted(spec: ScriptSpec) -> (Connection, tokio::task::JoinHandle<()>) {
        let (client, server) = tokio::net::UnixStream::pair().expect("pair");
        let server = tokio::spawn(async move {
            ScriptedServer::on_stream(server, spec).run().await;
        });
        (Connection::from_stream(client), server)
    }

    #[tokio::test]
    async fn reads_present_and_absent_keys_in_order() {
        let spec = ScriptSpec::new().stored_metadata(Scope::Global, "a/v1", b"1".to_vec());
        let (mut conn, server) = scripted(spec);
        let values = read(&mut conn, &["missing/v1", "a/v1"])
            .await
            .expect("read");
        drop(conn);
        server.await.expect("server");
        assert_eq!(values, [None, Some(b"1".to_vec())]);
    }

    #[tokio::test]
    async fn a_write_lands_before_it_returns() {
        let (mut conn, server) = scripted(ScriptSpec::new());
        write(&mut conn, "a/v1", b"2".to_vec())
            .await
            .expect("write");
        let values = read(&mut conn, &["a/v1"]).await.expect("read");
        drop(conn);
        server.await.expect("server");
        assert_eq!(values, [Some(b"2".to_vec())]);
    }

    #[tokio::test]
    async fn a_refused_write_is_an_error_and_a_refused_read_is_absent() {
        let spec = ScriptSpec::new().refuse_metadata(ErrorCode::PermissionDenied, "no");
        let (mut conn, server) = scripted(spec);
        let refused = write(&mut conn, "a/v1", b"2".to_vec()).await;
        assert!(
            matches!(refused, Err(WriteError::Refused(_))),
            "{refused:?}"
        );
        let values = read(&mut conn, &["a/v1"]).await.expect("read");
        assert_eq!(values, [None]);
        drop(conn);
        server.await.expect("server");
    }
}
