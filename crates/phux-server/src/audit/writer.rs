//! The append side: one open log, its chain head, and its signing key.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use base64::Engine as _;
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::Value;

use super::{
    AuditActor, AuditRecord, GENESIS, KEEP_ROTATED, KEY_FILE, Kind, LOG_FILE, Outcome,
    PUBLIC_KEY_FILE, ROTATE_BYTES, SCHEMA_VERSION,
};

/// An open log: the live file, the chain head, and the signing key.
pub struct AuditLog {
    dir: PathBuf,
    file: File,
    size: u64,
    next_seq: u64,
    prev: String,
    key: Ed25519KeyPair,
    host: String,
    pub(super) rotate_at: u64,
}

impl std::fmt::Debug for AuditLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditLog")
            .field("dir", &self.dir)
            .field("next_seq", &self.next_seq)
            .finish_non_exhaustive()
    }
}

impl AuditLog {
    /// Open (creating as needed) the log in `dir`, resuming its chain.
    ///
    /// # Errors
    ///
    /// The directory, key, or live file cannot be created or read.
    pub fn open(dir: &Path) -> io::Result<Self> {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        let key = load_or_create_key(dir)?;
        let path = dir.join(LOG_FILE);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .mode(0o600)
            .open(&path)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        let (last, torn) = last_record(&mut file)?;
        // A crash mid-write leaves a torn line; end it so the next record
        // starts clean (verify reports the torn one).
        if torn {
            file.write_all(b"\n")?;
        }
        // Just rotated: the chain head is the last record of `.1`.
        let last = match (last, File::open(rotated(dir, 1))) {
            (Some(last), _) => Some(last),
            (None, Ok(mut previous)) => last_record(&mut previous)?.0,
            (None, Err(_)) => None,
        };
        let size = file.metadata()?.len();
        let host = nix::unistd::gethostname()
            .ok()
            .and_then(|host| host.into_string().ok())
            .unwrap_or_default();
        Ok(Self {
            dir: dir.to_owned(),
            file,
            size,
            next_seq: last.as_ref().map_or(0, |last| last.seq.saturating_add(1)),
            prev: last.map_or_else(|| GENESIS.to_owned(), |last| last.hash),
            key,
            host,
            rotate_at: ROTATE_BYTES,
        })
    }

    /// Sign and append one record; the chain advances only once it is
    /// written.
    ///
    /// # Errors
    ///
    /// Rotation or the write failed.
    pub fn append(&mut self, entry: Entry) -> io::Result<()> {
        let mut record = AuditRecord {
            v: SCHEMA_VERSION,
            seq: self.next_seq,
            ts: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            host: self.host.clone(),
            kind: entry.kind,
            actor: entry.actor,
            action: entry.action,
            target: entry.target,
            outcome: entry.outcome,
            detail: entry.detail,
            prev: self.prev.clone(),
            hash: String::new(),
            sig: String::new(),
        };
        let digest = record.digest();
        record.hash = hex::encode(digest);
        record.sig =
            base64::engine::general_purpose::STANDARD.encode(self.key.sign(&digest).as_ref());
        let mut line = serde_json::to_vec(&record).map_err(io::Error::other)?;
        line.push(b'\n');
        let len = line.len() as u64;
        if self.size > 0 && self.size + len > self.rotate_at {
            self.rotate()?;
        }
        // One write per record, no fsync: the page cache survives a server
        // crash, and rotation and shutdown sync.
        self.file.write_all(&line)?;
        self.size += len;
        self.next_seq += 1;
        self.prev = record.hash;
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file.sync_all()?;
        for n in (1..KEEP_ROTATED).rev() {
            match std::fs::rename(rotated(&self.dir, n), rotated(&self.dir, n + 1)) {
                Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
                _ => {}
            }
        }
        let path = self.dir.join(LOG_FILE);
        std::fs::rename(&path, rotated(&self.dir, 1))?;
        self.file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&path)?;
        self.size = 0;
        File::open(&self.dir)?.sync_all()
    }

    /// Flush the live file to stable storage.
    ///
    /// # Errors
    ///
    /// The sync failed.
    pub fn sync(&self) -> io::Result<()> {
        self.file.sync_all()
    }
}

/// One record's caller-supplied fields.
#[derive(Debug)]
pub struct Entry {
    /// What it is about.
    pub kind: Kind,
    /// Who acted.
    pub actor: AuditActor,
    /// The command or frame name, or the event.
    pub action: String,
    /// What it acted on.
    pub target: Option<String>,
    /// How it came out.
    pub outcome: Outcome,
    /// Kind-specific facts (an object).
    pub detail: Value,
}

pub(super) fn rotated(dir: &Path, n: u32) -> PathBuf {
    dir.join(format!("{LOG_FILE}.{n}"))
}

fn load_or_create_key(dir: &Path) -> io::Result<Ed25519KeyPair> {
    let path = dir.join(KEY_FILE);
    let pkcs8 = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            let generated = Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
                .map_err(|_| io::Error::other("could not generate the audit key"))?;
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)?;
            file.write_all(generated.as_ref())?;
            file.sync_all()?;
            generated.as_ref().to_vec()
        }
        Err(err) => return Err(err),
    };
    let key = Ed25519KeyPair::from_pkcs8(&pkcs8)
        .map_err(|_| io::Error::other(format!("{} is not an ed25519 key", path.display())))?;
    let public = hex::encode(key.public_key().as_ref());
    let pub_path = dir.join(PUBLIC_KEY_FILE);
    if std::fs::read_to_string(&pub_path)
        .ok()
        .as_deref()
        .map(str::trim)
        != Some(public.as_str())
    {
        std::fs::write(&pub_path, format!("{public}\n"))?;
    }
    Ok(key)
}

/// The last complete record in `file`, and whether it ends in a torn line.
/// Reads only the tail: records are small.
fn last_record(file: &mut File) -> io::Result<(Option<AuditRecord>, bool)> {
    const TAIL: u64 = 256 * 1024;
    let len = file.metadata()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(TAIL)))?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail)?;
    let torn = tail.last().is_some_and(|byte| *byte != b'\n');
    let last = tail
        .split(|byte| *byte == b'\n')
        .rev()
        .skip(usize::from(torn))
        .find(|line| !line.is_empty())
        .and_then(|line| serde_json::from_slice(line).ok());
    Ok((last, torn))
}
