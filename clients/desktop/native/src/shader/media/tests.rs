use super::*;

#[test]
fn ppm_frames_read_as_rgba_until_a_clean_end() {
    let mut stream = b"P6\n2 1\n255\n".to_vec();
    stream.extend_from_slice(&[1, 2, 3, 4, 5, 6]);
    stream.extend_from_slice(b"P6 1 1 255\n");
    stream.extend_from_slice(&[7, 8, 9]);
    let mut reader = std::io::Cursor::new(stream);
    let (width, height, rgba) = read_ppm(&mut reader).expect("reads").expect("a frame");
    assert_eq!((width, height), (2, 1));
    assert_eq!(rgba, [1, 2, 3, 255, 4, 5, 6, 255]);
    let (_, _, rgba) = read_ppm(&mut reader).expect("reads").expect("a frame");
    assert_eq!(rgba, [7, 8, 9, 255]);
    assert!(read_ppm(&mut reader).expect("clean end").is_none());
}

#[test]
fn a_truncated_or_foreign_frame_is_an_error() {
    let mut short = std::io::Cursor::new(b"P6\n4 4\n255\n\x01\x02".to_vec());
    assert!(read_ppm(&mut short).is_err());
    let mut foreign = std::io::Cursor::new(b"P5\n1 1\n255\n\x01".to_vec());
    assert!(
        read_ppm(&mut foreign)
            .expect_err("grayscale")
            .contains("P5")
    );
    let mut deep = std::io::Cursor::new(b"P6\n1 1\n65535\n".to_vec());
    assert!(read_ppm(&mut deep).is_err());
}

#[test]
fn a_home_relative_source_expands() {
    let home = Path::new("/home/me");
    assert_eq!(
        expand("~/Movies/a.mp4", Some(home)),
        PathBuf::from("/home/me/Movies/a.mp4")
    );
    assert_eq!(expand("~", Some(home)), PathBuf::from("/home/me"));
    assert_eq!(
        expand("~other/a.mp4", Some(home)),
        PathBuf::from("~other/a.mp4")
    );
    assert_eq!(expand("clip.mp4", Some(home)), PathBuf::from("clip.mp4"));
}

#[test]
fn feeds_are_shared_per_source_and_a_missing_file_decodes_nothing() {
    let source = "/nonexistent/phux-shader-media-test.mp4";
    let one = feed(source);
    let two = feed(source);
    assert!(Arc::ptr_eq(&one, &two));
    assert_eq!(one.source(), source);
    std::thread::sleep(Duration::from_millis(200));
    assert!(one.latest().is_none());
}

/// Decodes a generated clip end to end when `ffmpeg` is installed.
#[test]
fn a_file_decodes_into_frames() {
    let Some(ffmpeg) = tool("ffmpeg") else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let dir = std::env::temp_dir().join(format!("phux-media-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let clip = dir.join("bars.mp4");
    let made = Command::new(ffmpeg)
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i"])
        .arg("testsrc=size=320x180:rate=30:duration=1")
        .args(["-pix_fmt", "yuv420p"])
        .arg(&clip)
        .status()
        .expect("runs ffmpeg");
    assert!(made.success());
    let feed = feed(&clip.to_string_lossy());
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut first = None;
    while Instant::now() < deadline {
        if let Some(picture) = feed.latest() {
            match first {
                None => first = Some(picture.serial),
                Some(serial) if picture.serial > serial => {
                    assert_eq!((picture.width, picture.height), (320, 180));
                    assert_eq!(picture.rgba.len(), 320 * 180 * 4);
                    drop(feed);
                    let _ = std::fs::remove_dir_all(&dir);
                    return;
                }
                Some(_) => {}
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("no frames decoded from {}", clip.display());
}
