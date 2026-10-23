use super::*;
use crate::updates::{run_bounded_with_progress, tests::scratch};
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

/// Force curl's records to straddle reads, including the decimal and CR.
struct Fragmented<'a>(&'a [u8]);

impl Read for Fragmented<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = buffer.len().min(2);
        self.0.read(&mut buffer[..count])
    }
}

#[test]
fn curl_records_are_floored_and_100_is_still_downloading() {
    assert_eq!(parse_record(b"  0.0%"), Some(StageProgress::Downloading(0)));
    assert_eq!(
        parse_record(b"#####  19.9%"),
        Some(StageProgress::Downloading(19))
    );
    assert_eq!(
        parse_record(b"###### 100.0%"),
        Some(StageProgress::Downloading(100))
    );
    assert_eq!(
        parse_record(b"phux-update: verifying"),
        Some(StageProgress::Verifying)
    );
    for record in [
        "-=O=-     #       #", // curl's unknown-content-length animation
        "curl: transfer failed at 50.0%",
        "50%",
        "50.55%",
        "101.0%",
        "100.1%",
        "nan%",
        "### -1.0%",
    ] {
        assert_eq!(parse_record(record.as_bytes()), None, "{record}");
    }
}

#[test]
fn stderr_stream_handles_cr_newline_duplicates_and_final_unterminated_error() {
    let (tx, rx) = mpsc::channel();
    let input = b"warning\n\r## 20.1%\r## 20.9%\r# 10.0%\r### 100.0%\nphux-update: verifying\n## 50.0%\rcurl: (18) partial file";
    let stderr = read_stderr(Fragmented(input), Some(tx)).unwrap();
    assert_eq!(stderr, b"warning\n\rcurl: (18) partial file");
    assert_eq!(
        rx.try_iter().collect::<Vec<_>>(),
        [
            StageProgress::Downloading(20),
            StageProgress::Downloading(100),
            StageProgress::Verifying,
        ]
    );
}

#[test]
fn non_progress_stderr_and_disconnected_receivers_do_not_change_execution() {
    let input = b"### 100.0%\rphux-update: verifying\nerror\n";
    assert_eq!(read_stderr(&input[..], None).unwrap(), input);
    let (tx, rx) = mpsc::channel();
    drop(rx);
    assert_eq!(read_stderr(&input[..], Some(tx)).unwrap(), b"error\n");
}

#[test]
fn unknown_content_length_never_synthesizes_a_percentage() {
    let (tx, rx) = mpsc::channel();
    let input = b"\r-=O=-     #       #\r-=O=-        #       #\r\nphux-update: verifying\n";
    read_stderr(Fragmented(input), Some(tx)).unwrap();
    assert_eq!(
        rx.try_iter().collect::<Vec<_>>(),
        [StageProgress::Verifying]
    );
}

#[test]
fn progress_arrives_before_a_live_command_exits_and_timeout_kills_its_group() {
    let dir = scratch("progress-timeout");
    let descendant = dir.join("survived");
    let (tx, rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let marker = descendant.clone();
    let worker = std::thread::spawn(move || {
        let result = run_bounded_with_progress(
            Command::new("/bin/sh")
                .args([
                    "-c",
                    "printf '\r#### 42.9%%\r' >&2; (sleep 1; touch \"$1\") & wait",
                    "progress-test",
                ])
                .arg(marker),
            None,
            Duration::from_millis(300),
            "download timed out",
            Some(tx),
        );
        done_tx.send(result).unwrap();
    });
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(2)).unwrap(),
        StageProgress::Downloading(42)
    );
    assert!(matches!(done_rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
    let error = done_rx
        .recv_timeout(Duration::from_secs(3))
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("download timed out"));
    worker.join().unwrap();
    std::thread::sleep(Duration::from_secs(1));
    assert!(
        !descendant.exists(),
        "timeout left a live downloader descendant"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_failed_transfer_keeps_stderr_and_never_claims_verification() {
    let (tx, rx) = mpsc::channel();
    let error = run_bounded_with_progress(
        Command::new("/bin/sh").args([
            "-c",
            "printf '\r#### 100.0%%\rcurl: (18) partial file\\n' >&2; exit 18",
        ]),
        None,
        Duration::from_secs(3),
        "timed out",
        Some(tx),
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "curl: (18) partial file");
    assert_eq!(
        rx.try_iter().collect::<Vec<_>>(),
        [StageProgress::Downloading(100)]
    );
}

#[test]
fn successful_commands_deliver_verification_and_leave_stdout_unchanged() {
    let (tx, rx) = mpsc::channel();
    let text = run_bounded_with_progress(
        Command::new("/bin/sh").args([
            "-c",
            "printf '\r#### 100.0%%\r\\nphux-update: verifying\\n' >&2; echo staged",
        ]),
        None,
        Duration::from_secs(3),
        "timed out",
        Some(tx),
    )
    .unwrap();
    assert_eq!(text, "staged\n");
    assert_eq!(
        rx.try_iter().collect::<Vec<_>>(),
        [StageProgress::Downloading(100), StageProgress::Verifying]
    );
}
