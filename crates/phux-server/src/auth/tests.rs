use super::*;
use std::os::unix::fs::PermissionsExt;

use std::fs::{self, OpenOptions};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

use chrono::{Duration, Utc};

fn secret(minted: &MintedCredential) -> Vec<u8> {
    hex::decode(minted.secret()).unwrap()
}

mod locking;
mod reload;
mod rotation;
mod store;
