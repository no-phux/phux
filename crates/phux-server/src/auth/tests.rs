use super::*;
use std::os::unix::fs::PermissionsExt;

use std::fs;
use std::path::Path;

use jiff::{SignedDuration, Timestamp};

use crate::secure_file::{WriteFault, fault, with_lock};

fn secret(minted: &MintedCredential) -> Vec<u8> {
    hex::decode(minted.secret()).unwrap()
}

mod locking;
mod reload;
mod rotation;
mod store;
