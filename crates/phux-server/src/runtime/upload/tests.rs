use super::*;
use phux_protocol::wire::frame::FileUploadAck;
use std::fs::File;
use std::time::SystemTime;
use tempfile::TempDir;

const LIMITS: UploadLimits = UploadLimits {
    max_bytes: DEFAULT_UPLOAD_MAX_BYTES,
    max_files: DEFAULT_UPLOAD_MAX_FILES,
    partial_ttl: PARTIAL_UPLOAD_TTL,
};

fn id(byte: u8) -> FileUploadId {
    FileUploadId::new([byte; 16]).expect("non-zero id")
}

fn chunk(
    upload_id: FileUploadId,
    offset: u64,
    data: &[u8],
    final_chunk: bool,
    sha256: Option<[u8; 32]>,
) -> PutFileChunk {
    PutFileChunk {
        upload_id,
        terminal_id: ResourceId::local(1),
        extension: "png".to_owned(),
        offset,
        data: data.to_vec(),
        final_chunk,
        sha256,
        principal: "owner".to_owned(),
    }
}

fn whole(upload_id: FileUploadId, data: &[u8]) -> PutFileChunk {
    chunk(upload_id, 0, data, true, Some(Sha256::digest(data).into()))
}

fn env_for(root: &Path) -> super::super::ServerEnv {
    super::super::ServerEnv {
        upload_dir: Some(root.to_owned()),
        ..super::super::ServerEnv::default()
    }
}

fn age(path: &Path, by: Duration) {
    let file = File::options().write(true).open(path).unwrap();
    file.set_modified(SystemTime::now() - by).unwrap();
}

/// An upload id belongs to the principal that created it: another can
/// neither append to it, finish it, nor find it for TRANSCRIBE.
#[test]
fn an_upload_id_is_bound_to_its_principal() {
    let temp = TempDir::new().unwrap();
    let upload_id = id(4);
    write_chunk(
        temp.path(),
        LIMITS,
        &chunk(upload_id, 0, b"mine", false, None),
    )
    .unwrap();

    let mut foreign = chunk(upload_id, 4, b" too", false, None);
    foreign.principal = "intruder".to_owned();
    let (code, message) = write_chunk(temp.path(), LIMITS, &foreign).unwrap_err();
    assert_eq!(code, ErrorCode::InvalidCommand);
    assert!(message.contains("another principal"), "{message}");

    let digest: [u8; 32] = Sha256::digest(b"mine").into();
    write_chunk(
        temp.path(),
        LIMITS,
        &chunk(upload_id, 4, b"", true, Some(digest)),
    )
    .unwrap();
    let mut replay = chunk(upload_id, 4, b"", true, Some(digest));
    replay.principal = "intruder".to_owned();
    assert_eq!(
        write_chunk(temp.path(), LIMITS, &replay).unwrap_err().0,
        ErrorCode::InvalidCommand,
        "a finished upload is not re-acked to another principal"
    );

    let env = env_for(temp.path());
    assert!(
        completed_upload_path(&env, upload_id, "owner")
            .unwrap()
            .is_some()
    );
    assert_eq!(
        completed_upload_path(&env, upload_id, "intruder").unwrap(),
        None
    );
}

/// A sidecar from before the binding names no principal and keeps
/// working for whoever resumes or transcribes it.
#[test]
fn a_legacy_sidecar_admits_any_principal() {
    let temp = TempDir::new().unwrap();
    let upload_id = id(5);
    let hex_id = hex::encode(upload_id.as_bytes());
    fs::write(
        temp.path().join(format!(".phux-upload-{hex_id}.meta")),
        "png",
    )
    .unwrap();
    fs::write(
        temp.path().join(format!("phux-upload-{hex_id}.png")),
        b"old",
    )
    .unwrap();
    let env = env_for(temp.path());
    assert!(
        completed_upload_path(&env, upload_id, "anyone")
            .unwrap()
            .is_some()
    );
    let mut retry = whole(upload_id, b"old");
    retry.principal = "anyone".to_owned();
    assert!(write_chunk(temp.path(), LIMITS, &retry).is_ok());
}

/// The directory holds at most `max_files` uploads: a new id past it is
/// refused with the remedy, while an existing upload still resumes.
#[test]
fn the_file_quota_refuses_new_uploads_but_not_resumes() {
    let temp = TempDir::new().unwrap();
    let limits = UploadLimits {
        max_files: 2,
        ..LIMITS
    };
    write_chunk(temp.path(), limits, &whole(id(1), b"one")).unwrap();
    write_chunk(temp.path(), limits, &chunk(id(2), 0, b"tw", false, None)).unwrap();
    let (code, message) = write_chunk(temp.path(), limits, &whole(id(3), b"three")).unwrap_err();
    assert_eq!(code, ErrorCode::ResourceExhausted);
    assert!(message.contains("PHUX_UPLOAD_MAX_FILES"), "{message}");
    let digest: [u8; 32] = Sha256::digest(b"two").into();
    write_chunk(
        temp.path(),
        limits,
        &chunk(id(2), 2, b"o", true, Some(digest)),
    )
    .unwrap();
    assert!(
        write_chunk(
            temp.path(),
            UploadLimits {
                max_files: 0,
                ..limits
            },
            &whole(id(3), b"three")
        )
        .is_ok(),
        "0 disables the limit"
    );
}

/// The directory holds at most `max_bytes`, across every upload.
#[test]
fn the_byte_quota_spans_every_upload() {
    let temp = TempDir::new().unwrap();
    let limits = UploadLimits {
        max_bytes: 10,
        ..LIMITS
    };
    write_chunk(temp.path(), limits, &whole(id(1), b"123456")).unwrap();
    let (code, message) = write_chunk(temp.path(), limits, &whole(id(2), b"12345")).unwrap_err();
    assert_eq!(code, ErrorCode::ResourceExhausted);
    assert!(message.contains("PHUX_UPLOAD_MAX_BYTES"), "{message}");
    write_chunk(temp.path(), limits, &whole(id(3), b"1234")).unwrap();
    // A replay of retained bytes adds nothing and is still answered.
    assert!(write_chunk(temp.path(), limits, &whole(id(1), b"123456")).is_ok());
}

/// Partial uploads untouched past the TTL are swept at the next upload
/// (part and sidecar); fresh partials and finished files are kept.
#[test]
fn stale_partials_are_swept_and_finished_files_kept() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    write_chunk(root, LIMITS, &chunk(id(1), 0, b"stale", false, None)).unwrap();
    write_chunk(root, LIMITS, &chunk(id(2), 0, b"fresh", false, None)).unwrap();
    write_chunk(root, LIMITS, &whole(id(3), b"done")).unwrap();
    let name = |byte: u8, suffix: &str| {
        let hex_id = hex::encode([byte; 16]);
        match suffix {
            "part" | "meta" => root.join(format!(".phux-upload-{hex_id}.{suffix}")),
            _ => root.join(format!("phux-upload-{hex_id}.{suffix}")),
        }
    };
    let old = PARTIAL_UPLOAD_TTL + Duration::from_secs(60);
    for path in [
        name(1, "part"),
        name(1, "meta"),
        name(3, "png"),
        name(3, "meta"),
    ] {
        age(&path, old);
    }

    write_chunk(root, LIMITS, &chunk(id(4), 0, b"next", false, None)).unwrap();
    assert!(!name(1, "part").exists() && !name(1, "meta").exists());
    assert!(name(2, "part").exists() && name(2, "meta").exists());
    assert!(name(3, "png").exists() && name(3, "meta").exists());
}

/// The startup sweep removes stale partials without creating a missing
/// directory.
#[test]
fn the_startup_sweep_removes_stale_partials() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("uploads");
    sweep_stale_partials_at_startup(&env_for(&root));
    assert!(!root.exists(), "a missing directory is not created");

    fs::create_dir(&root).unwrap();
    write_chunk(&root, LIMITS, &chunk(id(1), 0, b"stale", false, None)).unwrap();
    let hex_id = hex::encode([1_u8; 16]);
    let part = root.join(format!(".phux-upload-{hex_id}.part"));
    let meta = root.join(format!(".phux-upload-{hex_id}.meta"));
    age(&part, PARTIAL_UPLOAD_TTL + Duration::from_secs(1));
    age(&meta, PARTIAL_UPLOAD_TTL + Duration::from_secs(1));
    sweep_stale_partials_at_startup(&env_for(&root));
    assert!(!part.exists() && !meta.exists());
}

#[test]
fn upload_admission_counts_capacity_and_releases_failed_job_claim() {
    static BUDGET: UploadBudget = UploadBudget::new(2, 1024);
    let mut request = chunk(id(1), 0, b"a", false, None);
    request.data = Vec::with_capacity(2048);
    request.data.push(b'a');
    assert!(matches!(
        BUDGET.reserve(&request),
        Err(CommandResult::Error {
            code: ErrorCode::ResourceExhausted,
            ..
        })
    ));
    assert_eq!(BUDGET.jobs.available_permits(), 2);
    assert_eq!(BUDGET.payload.available_permits(), 1024);

    request.data = b"small".to_vec();
    let first = BUDGET.reserve(&request).expect("first worker");
    let second = BUDGET.reserve(&request).expect("second worker");
    assert!(
        BUDGET.reserve(&request).is_err(),
        "job count is independently bounded"
    );
    drop((first, second));
    assert_eq!(BUDGET.jobs.available_permits(), 2);
    assert_eq!(BUDGET.payload.available_permits(), 1024);
}

#[tokio::test]
async fn upload_admission_survives_cancellation_of_running_worker() {
    static BUDGET: UploadBudget = UploadBudget::new(1, 1024);
    let request = chunk(id(1), 0, b"payload", false, None);
    let reservation = BUDGET.reserve(&request).expect("worker admission");
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (finish_tx, finish_rx) = std::sync::mpsc::channel();
    let worker = tokio::task::spawn_blocking(move || {
        let _reservation = reservation;
        started_tx.send(()).expect("announce start");
        finish_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("finish worker");
    });
    started_rx.await.expect("worker started");
    worker.abort();
    assert!(
        BUDGET.reserve(&request).is_err(),
        "a running worker still owns admission"
    );
    finish_tx.send(()).expect("release worker");
    worker
        .await
        .expect("running blocking worker completes despite abort");
    assert!(BUDGET.reserve(&request).is_ok());
}

#[test]
fn chunks_finalize_only_after_digest_and_retry_idempotently() {
    let temp = TempDir::new().unwrap();
    let upload_id = id(1);
    let first = chunk(upload_id, 0, b"hello ", false, None);
    assert_eq!(
        write_chunk(temp.path(), LIMITS, &first).unwrap(),
        FileUploadAck {
            next_offset: 6,
            path: None
        }
    );
    assert_eq!(
        write_chunk(temp.path(), LIMITS, &first)
            .unwrap()
            .next_offset,
        6
    );

    let digest: [u8; 32] = Sha256::digest(b"hello world").into();
    let last = chunk(upload_id, 6, b"world", true, Some(digest));
    let ack = write_chunk(temp.path(), LIMITS, &last).unwrap();
    let path = PathBuf::from(ack.path.clone().unwrap());
    assert_eq!(ack.next_offset, 11);
    assert_eq!(fs::read(&path).unwrap(), b"hello world");
    assert_eq!(write_chunk(temp.path(), LIMITS, &last).unwrap(), ack);

    let mut changed_extension = chunk(upload_id, 6, b"world", true, Some(digest));
    changed_extension.extension = "jpg".to_owned();
    assert_eq!(
        write_chunk(temp.path(), LIMITS, &changed_extension)
            .unwrap_err()
            .0,
        ErrorCode::InvalidCommand
    );
}

#[test]
fn rejects_gaps_conflicts_traversal_and_bad_digest() {
    let temp = TempDir::new().unwrap();
    let upload_id = id(2);
    let gap = chunk(upload_id, 1, b"x", false, None);
    assert_eq!(
        write_chunk(temp.path(), LIMITS, &gap).unwrap_err().0,
        ErrorCode::InvalidCommand
    );

    let first = chunk(upload_id, 0, b"abc", false, None);
    write_chunk(temp.path(), LIMITS, &first).unwrap();
    let conflict = chunk(upload_id, 0, b"xbc", false, None);
    assert_eq!(
        write_chunk(temp.path(), LIMITS, &conflict).unwrap_err().0,
        ErrorCode::InvalidCommand
    );

    let mut traversal = chunk(id(3), 0, b"x", false, None);
    traversal.extension = "../png".to_owned();
    assert_eq!(
        write_chunk(temp.path(), LIMITS, &traversal).unwrap_err().0,
        ErrorCode::InvalidCommand
    );

    let bad_final = chunk(upload_id, 3, b"", true, Some([0; 32]));
    assert_eq!(
        write_chunk(temp.path(), LIMITS, &bad_final).unwrap_err().0,
        ErrorCode::InvalidCommand
    );
    assert!(
        !temp
            .path()
            .join("phux-upload-02020202020202020202020202020202.png")
            .exists()
    );
    assert!(
        !temp
            .path()
            .join(".phux-upload-02020202020202020202020202020202.part")
            .exists()
    );
    assert!(
        !temp
            .path()
            .join(".phux-upload-02020202020202020202020202020202.meta")
            .exists()
    );
}
