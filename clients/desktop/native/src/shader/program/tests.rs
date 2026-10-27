use super::*;

const FLAT: &str = "fn shade(uv: vec2f) -> vec4f {\n    return vec4f(uv, 0.0, 1.0);\n}\n";

#[test]
fn a_program_compiles_and_is_still_without_the_clock() {
    let program = Program::compile("flat", FLAT).expect("compiles");
    assert!(!program.animated);
    assert!(program.wgsl.contains("fn phux_fragment"));
    let again = Program::compile("flat", FLAT).expect("compiles");
    assert_eq!(program.hash, again.hash);
}

#[test]
fn reading_the_clock_animates_but_a_comment_does_not() {
    let timed = "fn shade(uv: vec2f) -> vec4f { return vec4f(sin(phux . time)); }";
    assert!(Program::compile("timed", timed).expect("compiles").animated);
    let framed = "fn shade(uv: vec2f) -> vec4f { return vec4f(phux.frame); }";
    assert!(
        Program::compile("framed", framed)
            .expect("compiles")
            .animated
    );
    let commented = "// phux.time\n/* phux.frame */\nfn shade(uv: vec2f) -> vec4f { return vec4f(phux.timeless_not_a_field_but_unused_here_is_not_valid); }";
    // An unknown member is an error, and it names the program's own line.
    let error = Program::compile("commented", commented).expect_err("unknown member");
    assert_eq!(error.at.map(|(line, _)| line), Some(3));
    let quiet = "// phux.time\nfn shade(uv: vec2f) -> vec4f { return phux.accent; }";
    assert!(!Program::compile("quiet", quiet).expect("compiles").animated);
}

#[test]
fn a_syntax_error_reports_the_programs_line_and_column() {
    let broken = "fn shade(uv: vec2f) -> vec4f {\n    let x = 1.0\n    return vec4f(x);\n}\n";
    let error = Program::compile("broken", broken).expect_err("missing semicolon");
    let (line, column) = error.at.expect("located");
    assert!((2..=3).contains(&line), "line {line}: {error}");
    assert!(column >= 1);
    assert!(error.to_string().starts_with(&format!("{line}:{column}: ")));
}

#[test]
fn a_type_error_is_located_in_the_program() {
    let wrong = "fn shade(uv: vec2f) -> vec4f {\n    return vec4f(uv, 0.0, 1.0) + uv;\n}\n";
    let error = Program::compile("wrong", wrong).expect_err("vec4 + vec2");
    assert_eq!(error.at.map(|(line, _)| line), Some(2), "{error}");
    assert!(
        error.message.contains("vec4f") && error.message.contains("vec2f"),
        "{error}"
    );
    assert!(!error.message.contains("Scalar {"), "{error}");
}

#[test]
fn naga_dumps_read_as_wgsl() {
    assert_eq!(
        tidy(
            "can't work with [3] (of type Vector { size: Quad, scalar: Scalar { kind: Float, width: 4 } }) and [0] (of type Scalar { kind: Sint, width: 4 })"
        ),
        "can't work with vec4f and i32"
    );
    assert_eq!(tidy("Expression [12] is invalid"), "Expression is invalid");
    assert_eq!(tidy("array[i] stays"), "array[i] stays");
    assert_eq!(
        tidy("[2] (of type Array { size: Constant(4) }) is bad"),
        "Array { size: Constant(4) } is bad"
    );
}

#[test]
fn a_missing_or_mistyped_shade_names_the_contract() {
    let none = "fn paint(uv: vec2f) -> vec4f { return vec4f(1.0); }";
    let error = Program::compile("none", none).expect_err("no shade");
    assert_eq!(error.at, None);
    assert!(
        error.message.contains("fn shade(uv: vec2f) -> vec4f"),
        "{error}"
    );
    let mistyped = "fn shade(uv: vec2f) -> vec3f { return vec3f(1.0); }";
    let error = Program::compile("mistyped", mistyped).expect_err("wrong return");
    assert!(
        error.message.contains("fn shade(uv: vec2f) -> vec4f"),
        "{error}"
    );
}

#[test]
fn programs_bring_no_bindings_or_entry_points() {
    let bound = "@group(0) @binding(1) var<uniform> mine: vec4f;\nfn shade(uv: vec2f) -> vec4f { return mine; }";
    let error = Program::compile("bound", bound).expect_err("binding");
    assert!(
        error.message.contains("`mine` declares a binding"),
        "{error}"
    );
    let entry = format!("{FLAT}@compute @workgroup_size(1) fn mine() {{}}");
    let error = Program::compile("entry", &entry).expect_err("entry point");
    assert!(
        error.message.contains("`mine` is an entry point"),
        "{error}"
    );
}

#[test]
fn an_oversized_program_is_refused_before_parsing() {
    let huge = format!("{FLAT}{}", "//".repeat(MAX_SOURCE_BYTES));
    let error = Program::compile("huge", &huge).expect_err("too big");
    assert!(error.message.contains("limit"), "{error}");
}

/// [`Uniforms::pack`] writes exactly the layout naga gives the prelude's
/// `Phux` struct.
#[test]
fn the_packed_block_matches_the_wgsl_layout() {
    let program = Program::compile("flat", FLAT).expect("compiles");
    let module = naga::front::wgsl::parse_str(&program.wgsl).expect("parses");
    let (_, phux) = module
        .types
        .iter()
        .find(|(_, ty)| ty.name.as_deref() == Some("Phux"))
        .expect("Phux struct");
    let naga::TypeInner::Struct { members, span } = &phux.inner else {
        panic!("Phux is a struct");
    };
    assert_eq!(*span as usize, UNIFORM_BYTES);
    let offset = |name: &str| {
        members
            .iter()
            .find(|member| member.name.as_deref() == Some(name))
            .map(|member| member.offset as usize)
            .expect(name)
    };
    let uniforms = Uniforms {
        size: [11., 12.],
        pointer: [13., 14.],
        time: 15.,
        frame: 16.,
        intensity: 17.,
        background: [21., 22., 23., 24.],
        success: [31., 32., 33., 34.],
        ansi: {
            let mut ansi = [[0.; 4]; 16];
            ansi[15] = [41., 42., 43., 44.];
            ansi
        },
        ..Uniforms::default()
    };
    let bytes = uniforms.pack();
    let at = |offset: usize| {
        f32::from_ne_bytes(bytes[offset..offset + 4].try_into().expect("four bytes"))
    };
    assert_eq!(at(offset("size") + 4), 12.);
    assert_eq!(at(offset("pointer")), 13.);
    assert_eq!(at(offset("time")), 15.);
    assert_eq!(at(offset("frame")), 16.);
    assert_eq!(at(offset("intensity")), 17.);
    assert_eq!(at(offset("background") + 12), 24.);
    assert_eq!(at(offset("success")), 31.);
    assert_eq!(at(offset("ansi") + 15 * 16 + 8), 43.);
}

#[test]
fn every_builtin_and_the_starter_compile() {
    for builtin in crate::shader::library::BUILTINS {
        let program = Program::compile(builtin.name, builtin.source)
            .unwrap_or_else(|error| panic!("{}: {error}", builtin.name));
        assert!(program.animated, "{} should move", builtin.name);
    }
    Program::compile("starter", crate::shader::library::STARTER).expect("starter compiles");
}
