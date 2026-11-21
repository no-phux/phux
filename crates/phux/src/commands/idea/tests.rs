use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::*;

const SECRET_HEX: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
const VECTOR_TS: &str = "1700000000";
const VECTOR_IDEM: &str = "018f6b3c-7c3a-7b2e-8c1d-4e5f60718293";
const VECTOR_BODY: &str = "ship the idea spool";
/// `openssl dgst -sha256 -mac HMAC -macopt hexkey:$SECRET_HEX` over
/// `ts \\n idem \\n sha256hex(body)`. Not computed by [`sign`].
const VECTOR_SIG: &str = "2e300946913db246e3d836ef23cc95d400b7122944b6e179dba59cad914c68da";

struct Hit {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

struct Mock {
    addr: std::net::SocketAddr,
    hits: Arc<Mutex<Vec<Hit>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Mock {
    fn spawn(status: u16, reason: &'static str, body: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let hits = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hits_thread = Arc::clone(&hits);
        let stop_thread = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            while !stop_thread.load(Ordering::SeqCst) {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                if stop_thread.load(Ordering::SeqCst) {
                    break;
                }
                if let Some(hit) = read_request(&mut stream) {
                    hits_thread.lock().expect("hits").push(hit);
                    let bytes = format!(
                        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(bytes.as_bytes());
                }
            }
        });
        Self {
            addr,
            hits,
            stop,
            thread: Some(thread),
        }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/ideas/capture", self.addr.port())
    }

    fn hits(&self) -> Vec<Hit> {
        self.hits.lock().expect("hits").drain(..).collect()
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_secs(1));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Option<Hit> {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("timeout");
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];
    let header_end = loop {
        if let Some(pos) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
            break pos;
        }
        if buf.len() > 64 * 1024 {
            return None;
        }
        match stream.read(&mut tmp) {
            Ok(0) | Err(_) => return None,
            Ok(read) => buf.extend_from_slice(&tmp[..read]),
        }
    };
    let head = std::str::from_utf8(&buf[..header_end]).ok()?;
    let mut lines = head.split("\r\n");
    let request = lines.next()?;
    let mut parts = request.split_whitespace();
    let method = parts.next()?.to_owned();
    let path = parts.next()?.to_owned();
    let mut headers = Vec::new();
    let mut content_length = 0usize;
    for line in lines {
        let (name, value) = line.split_once(':')?;
        let name = name.trim().to_owned();
        let value = value.trim().to_owned();
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value.parse().ok()?;
        }
        headers.push((name, value));
    }
    let mut body = buf[header_end + 4..].to_vec();
    while body.len() < content_length {
        match stream.read(&mut tmp) {
            Ok(0) | Err(_) => break,
            Ok(read) => body.extend_from_slice(&tmp[..read]),
        }
    }
    body.truncate(content_length);
    Some(Hit {
        method,
        path,
        headers,
        body,
    })
}

fn header<'a>(hit: &'a Hit, name: &str) -> &'a str {
    hit.headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map_or("", |(_, value)| value.as_str())
}

fn write_env(path: &Path, body: &str) {
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .expect("ops.env");
    file.write_all(body.as_bytes()).expect("write");
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .expect("chmod");
}

fn creds(dir: &Path) -> Creds {
    let path = dir.join("ops.env");
    write_env(
        &path,
        &format!(
            "# ideas capture\nexport PHUX_IDEAS_KEY_ID=\"cli-test\"\nPHUX_IDEAS_KEY='{SECRET_HEX}'\nPHUX_OPS_KEY=ignored\n"
        ),
    );
    load_creds(&path).expect("creds")
}

fn closed_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    format!("http://127.0.0.1:{port}/ideas/capture")
}

fn spool_files(spool: &Path) -> Vec<PathBuf> {
    let mut files: Vec<_> = fs::read_dir(spool)
        .expect("spool")
        .map(|entry| entry.expect("entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    files
}

#[test]
fn signature_on_the_wire_matches_the_known_vector() {
    let root = tempfile::tempdir().expect("temp");
    let creds = creds(root.path());
    assert!(!format!("{creds:?}").contains(SECRET_HEX));
    let spool = root.path().join("spool");
    fs::create_dir_all(&spool).expect("spool");
    let stored = Stored {
        idempotency_key: VECTOR_IDEM.to_owned(),
        body: VECTOR_BODY.to_owned(),
        permanent_error: None,
    };
    write_atomic(
        &spool.join(format!("{VECTOR_IDEM}.json")),
        &entry_bytes(&stored).expect("json"),
    )
    .expect("seed");
    let mock = Mock::spawn(201, "Created", "{\"id\":\"01ARZ3NDEKTSV4RRFFQ69G5FAV\"}");
    let report = deliver(&spool, &creds, &mock.url(), None, || {
        VECTOR_TS.parse().expect("ts")
    })
    .expect("deliver");
    assert_eq!(report.sent, vec!["01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned()]);
    assert!(report.permanent.is_empty());
    assert_eq!(report.queued, 0);
    assert!(spool_files(&spool).is_empty());
    let hits = mock.hits();
    assert_eq!(hits.len(), 1, "one post");
    let hit = &hits[0];
    assert_eq!(hit.method, "POST");
    assert_eq!(hit.path, "/ideas/capture");
    assert_eq!(hit.body, VECTOR_BODY.as_bytes());
    assert_eq!(header(hit, "Idempotency-Key"), VECTOR_IDEM);
    assert_eq!(header(hit, "x-ideas-key-id"), "cli-test");
    assert_eq!(header(hit, "x-ideas-ts"), VECTOR_TS);
    assert_eq!(header(hit, "x-ideas-sig"), VECTOR_SIG);
    assert_eq!(header(hit, "x-ideas-source"), "cli");
    assert!(
        !hit.headers
            .iter()
            .any(|(_, value)| value.contains(SECRET_HEX))
    );
}

#[test]
fn offline_capture_replays_exactly_once_when_the_network_returns() {
    let root = tempfile::tempdir().expect("temp");
    let creds = creds(root.path());
    let spool = root.path().join("ideas").join("spool");
    let body = "remember the milk";
    let queued = deliver(&spool, &creds, &closed_url(), Some(body), unix_now).expect("queue");
    assert!(queued.sent.is_empty());
    assert_eq!(queued.queued, 1);
    let files = spool_files(&spool);
    assert_eq!(files.len(), 1);
    let mode = fs::metadata(&files[0]).expect("meta").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let dir_mode = fs::metadata(&spool).expect("meta").permissions().mode() & 0o777;
    assert_eq!(dir_mode, 0o700);
    let entry = load_entry(&files[0]).expect("entry");
    assert_eq!(entry.stored.body, body);
    let parsed = Uuid::parse_str(&entry.stored.idempotency_key).expect("uuid");
    assert_eq!(parsed.get_version(), Some(uuid::Version::SortRand));
    let key = entry.stored.idempotency_key;

    let mock = Mock::spawn(200, "OK", "{\"id\":\"01REPLAYCAPTUREID000000000\"}");
    let sent = deliver(&spool, &creds, &mock.url(), None, || 1_700_000_100).expect("replay");
    assert_eq!(sent.sent, vec!["01REPLAYCAPTUREID000000000".to_owned()]);
    assert_eq!(sent.queued, 0);
    let leftovers: Vec<_> = fs::read_dir(&spool).expect("spool").collect();
    assert!(
        leftovers.is_empty(),
        "scratch files are removed after the post"
    );
    let hits = mock.hits();
    assert_eq!(hits.len(), 1, "the queued idea is posted once");
    assert_eq!(header(&hits[0], "Idempotency-Key"), key);
    assert_eq!(hits[0].body, body.as_bytes());
    assert_eq!(header(&hits[0], "x-ideas-ts"), "1700000100");
    assert_eq!(header(&hits[0], "x-ideas-sig").len(), 64);

    let again = deliver(&spool, &creds, &mock.url(), None, || 1_700_000_200).expect("empty");
    assert!(again.sent.is_empty());
    assert_eq!(again.queued, 0);
    assert!(
        mock.hits().is_empty(),
        "a flushed spool does not send again"
    );
}

#[test]
fn idempotency_conflict_is_not_retried() {
    let root = tempfile::tempdir().expect("temp");
    let creds = creds(root.path());
    let spool = root.path().join("spool");
    fs::create_dir_all(&spool).expect("spool");
    let stored = Stored {
        idempotency_key: VECTOR_IDEM.to_owned(),
        body: "different body".to_owned(),
        permanent_error: None,
    };
    write_atomic(
        &spool.join(format!("{VECTOR_IDEM}.json")),
        &entry_bytes(&stored).expect("json"),
    )
    .expect("seed");
    let mock = Mock::spawn(409, "Conflict", "{\"error\":\"idempotency conflict\"}");
    let first = deliver(&spool, &creds, &mock.url(), None, unix_now).expect("conflict");
    assert!(first.sent.is_empty());
    assert_eq!(first.permanent.len(), 1);
    assert!(first.permanent[0].contains("409"));
    assert!(first.permanent[0].contains("will not be retried"));
    assert!(!first.permanent[0].contains("different body"));
    let entry = load_entry(&spool.join(format!("{VECTOR_IDEM}.json"))).expect("kept");
    assert_eq!(
        entry.stored.permanent_error.as_deref(),
        Some(CONFLICT_REASON)
    );
    assert_eq!(mock.hits().len(), 1);

    let second = deliver(&spool, &creds, &mock.url(), None, unix_now).expect("skip");
    assert!(second.sent.is_empty());
    assert_eq!(second.permanent.len(), 1);
    assert!(
        mock.hits().is_empty(),
        "a 409 stays in the spool and is not posted again"
    );
}

#[test]
fn private_tag_is_ensured_and_oversized_ideas_are_refused() {
    assert_eq!(
        prepare_body("keep this", true).expect("private"),
        "#private\nkeep this"
    );
    assert_eq!(
        prepare_body("already #private here", true).expect("present"),
        "already #private here"
    );
    assert_eq!(prepare_body("", true).expect("tag only"), "#private");
    assert!(prepare_body("", false).is_err());
    let max = "a".repeat(MAX_BODY_BYTES);
    assert_eq!(
        prepare_body(&max, false).expect("limit").len(),
        MAX_BODY_BYTES
    );
    assert!(prepare_body(&format!("{max}b"), false).is_err());
    assert!(prepare_body(&max, true).is_err());
}

#[test]
fn signature_is_not_on_the_curl_argv() {
    let root = tempfile::tempdir().expect("temp");
    let creds = creds(root.path());
    let spool = root.path().join("ideas").join("spool");
    ensure_private_dir(&spool).expect("spool");
    let signature = sign(
        &creds.secret,
        VECTOR_TS,
        VECTOR_IDEM,
        VECTOR_BODY.as_bytes(),
    );
    assert_eq!(signature, VECTOR_SIG);
    let headers = [
        ("Idempotency-Key", VECTOR_IDEM),
        ("x-ideas-key-id", creds.key_id.as_str()),
        ("x-ideas-ts", VECTOR_TS),
        ("x-ideas-sig", signature.as_str()),
        ("x-ideas-source", SOURCE_CLI),
    ];
    let prepared = prepare_curl(
        &spool,
        "http://127.0.0.1:9/ideas/capture",
        "=http",
        &headers,
        VECTOR_BODY.as_bytes(),
    )
    .expect("prepare");
    let args: Vec<String> = prepared
        .command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let flat = args.join("\n");
    assert!(
        !flat.contains(&signature),
        "the signature must not be a curl argument"
    );
    assert!(
        !flat.contains(SECRET_HEX),
        "the secret must not be a curl argument"
    );
    assert!(!flat.contains(VECTOR_BODY));
    let header_arg = args
        .windows(2)
        .find(|pair| pair[0] == "-H" && pair[1].starts_with('@'))
        .map(|pair| pair[1].as_str())
        .expect("signature headers are passed as -H @file");
    let header_path = Path::new(&header_arg[1..]);
    let paths = [
        prepared.body.path.as_path(),
        prepared.response.path.as_path(),
        header_path,
    ];
    for path in paths {
        assert_eq!(path.parent(), Some(spool.as_path()));
        let meta = path.symlink_metadata().expect("scratch meta");
        assert!(meta.file_type().is_file());
        assert_eq!(
            meta.permissions().mode() & 0o777,
            0o600,
            "{}",
            path.display()
        );
    }
    let header_text = fs::read_to_string(header_path).expect("headers");
    assert!(header_text.contains(&signature));
    assert!(header_text.contains("x-ideas-sig: "));
    assert_eq!(
        fs::read(&prepared.body.path).expect("body"),
        VECTOR_BODY.as_bytes()
    );
}

#[test]
fn a_bad_secret_is_not_printed() {
    let root = tempfile::tempdir().expect("temp");
    let path = root.path().join("ops.env");
    let sentinel = "z".repeat(64);
    write_env(
        &path,
        &format!("PHUX_IDEAS_KEY_ID=cli-test\nPHUX_IDEAS_KEY={sentinel}\n"),
    );
    let err = load_creds(&path).expect_err("bad secret");
    let rendered = err.to_string();
    assert!(rendered.contains("64 hex"));
    assert!(rendered.contains("not printed"));
    assert!(!rendered.contains(&sentinel));
    assert!(!format!("{err:?}").contains(&sentinel));
}

#[test]
fn missing_keys_name_the_file_and_not_a_value() {
    let root = tempfile::tempdir().expect("temp");
    let path = root.path().join("ops.env");
    let err = load_creds(&path).expect_err("missing file");
    assert!(err.to_string().contains("PHUX_IDEAS_KEY_ID"));
    assert!(err.to_string().contains("PHUX_IDEAS_KEY"));
    assert!(err.to_string().contains(path.to_str().expect("utf8")));
    write_env(&path, "PHUX_IDEAS_KEY_ID=cli-test\n");
    let err = load_creds(&path).expect_err("missing secret");
    assert!(err.to_string().contains("PHUX_IDEAS_KEY"));
    assert!(!err.to_string().contains("PHUX_IDEAS_KEY_ID and"));
}

#[test]
fn idea_command_parses_and_does_not_take_the_server_socket() {
    let cli = crate::parse_cli(["phux", "idea", "--private", "a note"]).expect("parse");
    let command = cli.command.expect("verb");
    assert!(matches!(
        command,
        crate::commands::Command::Idea(crate::commands::IdeaArgs {
            private: true,
            flush: false,
            ref text,
        }) if text.as_deref() == Some("a note")
    ));
    assert_eq!(crate::commands::socketless_verb(&command), Some("idea"));
    let flush = crate::parse_cli(["phux", "idea", "--flush"]).expect("flush");
    assert!(matches!(
        flush.command,
        Some(crate::commands::Command::Idea(crate::commands::IdeaArgs {
            flush: true,
            text: None,
            ..
        }))
    ));
}
