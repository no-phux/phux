use super::*;
use std::time::{Duration, Instant};

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_owned()).collect()
}

fn finish(
    presentation: &mut Presentation,
    pool: &mut Pool,
    viewer: &Viewer,
    source: &str,
    width: u16,
) {
    let started = Instant::now();
    loop {
        let (_, waiting) = presentation.poll(pool, viewer, source, width);
        if !waiting {
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn mime_routing_is_exact_and_source_is_an_adapter() {
    assert!(is_markdown(Some(" Text/Markdown; charset=utf-8")));
    for value in [None, Some("text/html"), Some("text/markdown-unsafe")] {
        assert!(!is_markdown(value));
    }
    assert_eq!(
        Viewer::configured(Some(&[]), Path::new("/app/phux-rune")),
        Viewer::Source
    );
    let mut pool = Pool::default();
    let mut p = Presentation::default();
    finish(&mut p, &mut pool, &Viewer::Source, "**source**", 80);
    assert!(p.source);
    assert_eq!(p.text("**source**"), "**source**");
    assert!(pool.workers.is_empty());
}

#[test]
fn an_unresolved_app_path_never_selects_a_working_directory_executable() {
    for path in ["", "phux-rune", "./phux-rune"] {
        assert_eq!(Viewer::configured(None, Path::new(path)), Viewer::Source);
    }
}

#[test]
fn configured_argv_gets_only_stdin_data_and_width_not_shell_expansion() {
    let source = "# $(touch /never) `echo nope`\n";
    let output = process::render(&argv(&["/bin/cat"]), source, 72).unwrap();
    assert_eq!(output, source);
    let width = process::render(
        &argv(&["/usr/bin/printf", "%s", "{width}; $(echo nope)"]),
        "",
        42,
    )
    .unwrap();
    assert_eq!(width, "42; $(echo nope)");
    let large = "a".repeat(256 * 1024);
    assert_eq!(
        process::render(&argv(&["/bin/cat"]), &large, 80).unwrap(),
        large
    );
}

#[test]
fn failures_are_bounded_and_leave_source_available() {
    for command in [
        argv(&["/no-such-phux-viewer"]),
        argv(&[""]),
        argv(&["/bin/sh", "-c", "printf 'bad renderer' >&2; exit 4"]),
        argv(&["/bin/sh", "-c", "printf '\\377'"]),
        argv(&[
            "/bin/sh",
            "-c",
            "while :; do printf 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx'; done",
        ]),
    ] {
        assert!(
            process::render(&command, "# hello", 80).is_err(),
            "{command:?}"
        );
    }
    let started = Instant::now();
    let result = process::test_run(
        &argv(&["/bin/sleep", "5"]),
        "",
        80,
        Duration::from_millis(40),
    );
    assert!(result.unwrap_err().contains("timed out"));
    assert!(started.elapsed() < Duration::from_secs(2));
    let mut p = Presentation::default();
    finish(
        &mut p,
        &mut Pool::default(),
        &Viewer::Command(argv(&["/missing-viewer"])),
        "# original",
        80,
    );
    assert_eq!(p.text("# original"), "# original");
    assert!(p.status().contains("showing source"));
}

#[test]
fn render_results_toggle_resize_and_retirement_are_local_to_the_publication() {
    let mut pool = Pool::default();
    let adapter = Viewer::Command(argv(&["/usr/bin/printf", "width={width}"]));
    let mut p = Presentation::default();
    finish(&mut p, &mut pool, &adapter, "# source", 80);
    assert_eq!(p.text("# source"), "width=80");
    p.toggle();
    assert_eq!(p.text("# source"), "# source");
    assert_eq!(p.poll(&mut pool, &adapter, "# source", 60), (false, false));
    p.toggle();
    finish(&mut p, &mut pool, &adapter, "# source", 60);
    assert_eq!(p.text("# source"), "width=60");
    let mut next = Presentation::new(p.source);
    assert_eq!(next.text("# new"), "# new");
    finish(
        &mut next,
        &mut pool,
        &Viewer::Command(argv(&["/bin/cat"])),
        "# new",
        60,
    );
    assert_eq!(next.text("# new"), "# new");
}

#[test]
fn retired_jobs_still_count_against_admission() {
    let mut pool = Pool::default();
    let slow = Viewer::Command(argv(&["/bin/sleep", "0.1"]));
    drop(pool.start(slow.clone(), "".into(), 80).unwrap());
    drop(pool.start(slow.clone(), "".into(), 80).unwrap());
    assert!(pool.start(slow, "".into(), 80).is_none());
    let mut p = Presentation::default();
    finish(
        &mut p,
        &mut pool,
        &Viewer::Command(argv(&["/bin/cat"])),
        "latest",
        80,
    );
    assert_eq!(p.text("latest"), "latest");
}

#[test]
fn orderly_shutdown_joins_renderers_before_returning() {
    let mut pool = Pool::default();
    let flight = pool
        .start(
            Viewer::Command(argv(&["/bin/sleep", "0.05"])),
            "".into(),
            80,
        )
        .unwrap();
    drop(pool);
    assert_eq!(flight.0.try_recv().unwrap().unwrap(), "");
}

#[test]
fn descendant_output_cannot_keep_captures_or_workers_open() {
    for script in ["yes &", "yes & wait"] {
        let started = Instant::now();
        let result = process::test_run(
            &argv(&["/bin/sh", "-c", script]),
            "",
            80,
            Duration::from_millis(40),
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        if let Ok(output) = result {
            assert!(output.len() <= 2 * 1024 * 1024);
        }
    }
}

#[test]
#[ignore = "requires the digest-verified bundled tmd; set TMD_TEST_BIN"]
fn pinned_tmd_renders_markdown_through_the_actual_adapter() {
    let binary = std::env::var("TMD_TEST_BIN").expect("TMD_TEST_BIN");
    let executable = Path::new(&binary).with_file_name("phux-rune");
    let Viewer::Command(command) = Viewer::configured(None, &executable) else {
        panic!("desktop adapter")
    };
    assert_eq!(command[0], binary, "the packaged sibling, not PATH");
    let output = process::render(
        &command,
        "# Pancakes\n\nWhisk until **smooth**.\n\n| A | B |\n|---|---|\n| x | y |\n",
        60,
    )
    .unwrap();
    assert!(output.contains("Pancakes") && output.contains("Whisk until smooth."));
    assert!(output.contains('┌') && output.contains('│'));
    assert!(!output.contains("**") && !output.contains("# Pancakes") && !output.contains('\x1b'));
}
