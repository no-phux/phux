//! Read a retained operation receipt without repeating its effects.

use rusqlite::{Connection, OptionalExtension as _, params};
use serde::de::DeserializeOwned;

use crate::{error::Error, id::OperationId};

pub(crate) fn replay<T: DeserializeOwned>(
    conn: &Connection,
    operation_id: &OperationId,
    digest: &str,
) -> Result<Option<T>, Error> {
    match load_operation(conn, operation_id)? {
        None => Ok(None),
        Some((stored, result)) if stored == digest => Ok(Some(serde_json::from_str(&result)?)),
        Some(_) => Err(Error::OperationConflict {
            operation_id: operation_id.to_hex(),
        }),
    }
}

pub(crate) fn load_operation(
    conn: &Connection,
    operation_id: &OperationId,
) -> Result<Option<(String, String)>, Error> {
    Ok(conn
        .query_row(
            "SELECT digest, result_json FROM operation WHERE id = ?1",
            params![operation_id.as_slice()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?)
}
