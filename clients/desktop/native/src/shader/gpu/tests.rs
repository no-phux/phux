use super::*;

/// A renderer, or `None` on a machine with no usable GPU (CI without one).
fn renderer() -> Option<Renderer> {
    match Renderer::new() {
        Ok(renderer) => Some(renderer),
        Err(error) => {
            eprintln!("skipping: {error}");
            None
        }
    }
}

fn pixel(frame: &Frame, x: u32, y: u32) -> [u8; 4] {
    let at = ((y * frame.width + x) * 4) as usize;
    frame.bgra[at..at + 4].try_into().expect("four bytes")
}

#[test]
fn a_frame_reads_back_as_straight_bgra_with_uv_from_the_top_left() {
    let Some(mut renderer) = renderer() else {
        return;
    };
    let program = Program::compile(
        "uv",
        "fn shade(uv: vec2f) -> vec4f { return vec4f(uv.x, uv.y, phux.accent.b, 1.0); }",
    )
    .expect("compiles");
    let uniforms = Uniforms {
        accent: [0., 0., 1., 1.],
        intensity: 0.5,
        ..Uniforms::default()
    };
    let frame = renderer
        .render(&program, &uniforms, 100, 40)
        .expect("renders");
    assert_eq!((frame.width, frame.height), (100, 40));
    assert_eq!(frame.bgra.len(), 100 * 40 * 4);
    let [b, g, r, a] = pixel(&frame, 0, 0);
    assert!(r < 4 && g < 8, "top left is uv 0,0: {r} {g}");
    assert_eq!(b, 255);
    assert!((126..=129).contains(&a), "intensity halves alpha: {a}");
    let [_, g, r, _] = pixel(&frame, 99, 39);
    assert!(r > 250 && g > 248, "bottom right is uv 1,1: {r} {g}");
}

#[test]
fn a_new_program_or_size_replaces_the_pipeline_and_target() {
    let Some(mut renderer) = renderer() else {
        return;
    };
    let red = Program::compile(
        "red",
        "fn shade(uv: vec2f) -> vec4f { return vec4f(1.0, 0.0, 0.0, 1.0); }",
    )
    .expect("compiles");
    let clear = Program::compile(
        "clear",
        "fn shade(uv: vec2f) -> vec4f { return vec4f(0.0); }",
    )
    .expect("compiles");
    let uniforms = Uniforms::default();
    let frame = renderer.render(&red, &uniforms, 8, 8).expect("renders");
    assert_eq!(pixel(&frame, 3, 3), [0, 0, 255, 255]);
    let frame = renderer
        .render(&clear, &uniforms, 5000, 3)
        .expect("renders");
    assert_eq!((frame.width, frame.height), (MAX_SIDE, 3));
    assert_eq!(pixel(&frame, MAX_SIDE - 1, 2), [0, 0, 0, 0]);
}

/// A program's media reaches `phux_media` once its feed decodes a frame.
#[test]
fn a_programs_media_is_sampled_once_it_decodes() {
    let Some(mut renderer) = renderer() else {
        return;
    };
    let Some(ffmpeg) = media::tool("ffmpeg") else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let dir = std::env::temp_dir().join(format!("phux-gpu-media-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let clip = dir.join("red.mp4");
    let made = std::process::Command::new(ffmpeg)
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i"])
        .arg("color=c=red:size=64x36:rate=30:duration=1")
        .args(["-pix_fmt", "yuv420p"])
        .arg(&clip)
        .status()
        .expect("runs ffmpeg");
    assert!(made.success());
    let source = format!(
        "// phux:media {}\nfn shade(uv: vec2f) -> vec4f {{\n    return vec4f(phux_media(phux_media_cover(uv)).rgb, phux.media);\n}}\n",
        clip.display()
    );
    let program = Program::compile("red", &source).expect("compiles");
    let uniforms = Uniforms::default();
    let blank = renderer
        .render(&program, &uniforms, 16, 9)
        .expect("renders");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut frame = blank;
    while pixel(&frame, 8, 4)[3] == 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        frame = renderer
            .render(&program, &uniforms, 16, 9)
            .expect("renders");
    }
    let [b, g, r, a] = pixel(&frame, 8, 4);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(a, 255, "a frame is bound");
    assert!(r > 200 && g < 60 && b < 60, "the clip is red: {r} {g} {b}");
}
