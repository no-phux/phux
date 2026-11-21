//! The parity contract, spot-checked: a few of the golden vectors
//! (`tokens/derive-vectors.subset.json`, which `just design-gen` cuts from
//! the full set its tests check).
//! Colours match as exact `#RRGGBBAA` strings and contrast within 1e-12.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::many_single_char_names,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::missing_const_for_fn,
    reason = "a test harness spelling the vectors out field by field"
)]

use phux_design::solve::{contrast, fade, mix, on_fill, over, reach};
use phux_design::{
    Agent, Link, Mode, Params, Rgb8, Rgba8, Roles, Theme, ToneInput, contrast_more, derive, plural,
    tone_of,
};
use serde_json::Value;

const VECTORS: &str = include_str!("../tokens/derive-vectors.subset.json");

fn vectors() -> Value {
    serde_json::from_str(VECTORS).unwrap()
}

fn c(v: &Value) -> Rgb8 {
    Rgb8::parse(v.as_str().unwrap()).unwrap()
}

fn lut() -> &'static [u64] {
    Params::house().srgb_lut
}

#[test]
fn the_vectors_are_for_these_params() {
    let v = vectors();
    assert_eq!(v["solver"], "phux-derive/2");
    assert_eq!(v["params_sha256"], phux_design::tokens::META.params_sha256);
}

#[test]
fn primitives_match() {
    let v = vectors();
    let p = &v["primitives"];
    let n = Params::house().iterations;
    for case in p["contrast"].as_array().unwrap() {
        let got = contrast(c(&case["a"]), c(&case["b"]), lut());
        let want = case["expect"].as_f64().unwrap();
        assert!((got - want).abs() <= 1e-12, "{case}: {got}");
    }
    for case in p["mix"].as_array().unwrap() {
        let got = mix(c(&case["a"]), c(&case["b"]), case["t"].as_f64().unwrap());
        assert_eq!(got.to_string(), case["expect"], "{case}");
    }
    for case in p["over"].as_array().unwrap() {
        let top = Rgba8::parse(case["top"].as_str().unwrap()).unwrap();
        assert_eq!(
            over(top, c(&case["bottom"])).to_string(),
            case["expect"],
            "{case}"
        );
    }
    let surfaces = |case: &Value| {
        case["S"]
            .as_array()
            .unwrap()
            .iter()
            .map(c)
            .collect::<Vec<_>>()
    };
    for case in p["reach"].as_array().unwrap() {
        let got = reach(
            c(&case["c"]),
            c(&case["toward"]),
            case["bar"].as_f64().unwrap(),
            &surfaces(case),
            lut(),
            n,
        );
        assert_eq!(got.to_string(), case["expect"], "{case}");
    }
    for case in p["fade"].as_array().unwrap() {
        let got = fade(
            c(&case["c"]),
            c(&case["toward"]),
            case["target"].as_f64().unwrap(),
            &surfaces(case),
            lut(),
            n,
        );
        assert_eq!(got.to_string(), case["expect"], "{case}");
    }
    for case in p["on_fill"].as_array().unwrap() {
        assert_eq!(
            on_fill(c(&case["f"]), lut()).to_string(),
            case["expect"],
            "{case}"
        );
    }
}

fn input(v: &Value) -> Theme {
    let opt = |k: &str| v[k].as_str().map(|s| Rgb8::parse(s).unwrap());
    let mut ansi = [Rgb8::BLACK; 16];
    for (slot, x) in ansi.iter_mut().zip(v["ansi"].as_array().unwrap()) {
        *slot = c(x);
    }
    Theme {
        bg: c(&v["bg"]),
        fg: c(&v["fg"]),
        ansi,
        cursor: opt("cursor"),
        accent: opt("accent"),
        selection_bg: opt("selection_bg"),
        dark: v["dark"].as_bool(),
    }
}

/// The roles as the vectors spell them: a JSON object of hex strings.
fn as_json(r: &Roles) -> Value {
    // The generated groups are flat `Rgba` fields; their Debug form lists
    // each one, so the comparison is per role and names the field.
    let mut out = serde_json::Map::new();
    out.insert("mode".into(), Value::from(r.mode.key()));
    let mut put = |path: &str, c: Rgba8| {
        let (group, rest) = path.split_once('.').unwrap();
        let entry = out
            .entry(group)
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        let mut node = entry;
        let parts: Vec<&str> = rest.split('.').collect();
        for part in &parts[..parts.len() - 1] {
            node = node
                .as_object_mut()
                .unwrap()
                .entry(*part)
                .or_insert_with(|| Value::Object(serde_json::Map::new()));
        }
        node.as_object_mut()
            .unwrap()
            .insert(parts[parts.len() - 1].into(), Value::from(c.to_hex()));
    };
    let l = &r.layer;
    for (k, v) in [
        ("ground", l.ground),
        ("sidebar", l.sidebar),
        ("raised", l.raised),
        ("overlay", l.overlay),
        ("scrim", l.scrim),
    ] {
        put(&format!("layer.{k}"), v);
    }
    let t = &r.text;
    for (k, v) in [
        ("text", t.text),
        ("dim", t.dim),
        ("faint", t.faint),
        ("inverse", t.inverse),
    ] {
        put(&format!("text.{k}"), v);
    }
    let b = &r.border;
    for (k, v) in [
        ("muted", b.muted),
        ("base", b.base),
        ("strong", b.strong),
        ("control", b.control),
        ("focus", b.focus),
    ] {
        put(&format!("border.{k}"), v);
    }
    let w = &r.wash;
    for (k, v) in [
        ("hover", w.hover),
        ("press", w.press),
        ("current", w.current),
        ("selected", w.selected),
    ] {
        put(&format!("wash.{k}"), v);
    }
    let a = &r.accent;
    for (k, v) in [
        ("accent", a.accent),
        ("on_accent", a.on_accent),
        ("accent_text", a.accent_text),
    ] {
        put(&format!("accent.{k}"), v);
    }
    for tone in phux_design::Tone::ALL {
        let ink = r.tone(tone);
        let name = tone.key();
        put(&format!("status.{name}"), ink.ink);
        put(&format!("status.{name}_wash"), ink.wash);
        put(&format!("status.{name}_edge"), ink.edge);
        put(&format!("status.{name}_glow"), ink.glow);
    }
    put("selection.bg", r.selection.bg);
    put("selection.fg", r.selection.fg);
    let m = &r.r#match;
    for (k, v) in [
        ("bg", m.bg),
        ("current_bg", m.current_bg),
        ("current_fg", m.current_fg),
    ] {
        put(&format!("match.{k}"), v);
    }
    let d = &r.diff;
    for (k, v) in [
        ("add", d.add),
        ("remove", d.remove),
        ("add_text", d.add_text),
        ("remove_text", d.remove_text),
    ] {
        put(&format!("diff.{k}"), v);
    }
    for layer in phux_design::Layer::ALL {
        let f = r.flat(layer);
        let lk = layer.key();
        put(&format!("flat.{lk}.layer.scrim"), f.layer.scrim);
        for (k, v) in [
            ("muted", f.border.muted),
            ("base", f.border.base),
            ("strong", f.border.strong),
        ] {
            put(&format!("flat.{lk}.border.{k}"), v);
        }
        for (k, v) in [
            ("hover", f.wash.hover),
            ("press", f.wash.press),
            ("current", f.wash.current),
            ("selected", f.wash.selected),
        ] {
            put(&format!("flat.{lk}.wash.{k}"), v);
        }
        let s = &f.status;
        for (k, v) in [
            ("needs_you_wash", s.needs_you_wash),
            ("needs_you_edge", s.needs_you_edge),
            ("needs_you_glow", s.needs_you_glow),
            ("working_wash", s.working_wash),
            ("working_edge", s.working_edge),
            ("working_glow", s.working_glow),
            ("done_wash", s.done_wash),
            ("done_edge", s.done_edge),
            ("done_glow", s.done_glow),
            ("failed_wash", s.failed_wash),
            ("failed_edge", s.failed_edge),
            ("failed_glow", s.failed_glow),
            ("idle_wash", s.idle_wash),
            ("idle_edge", s.idle_edge),
            ("idle_glow", s.idle_glow),
            ("unknown_wash", s.unknown_wash),
            ("unknown_edge", s.unknown_edge),
            ("unknown_glow", s.unknown_glow),
            ("bell_wash", s.bell_wash),
            ("bell_edge", s.bell_edge),
            ("bell_glow", s.bell_glow),
        ] {
            put(&format!("flat.{lk}.status.{k}"), v);
        }
        put(&format!("flat.{lk}.match.bg"), f.r#match.bg);
        put(&format!("flat.{lk}.diff.add"), f.diff.add);
        put(&format!("flat.{lk}.diff.remove"), f.diff.remove);
    }
    out.insert(
        "syntax".into(),
        Value::Array(r.syntax.iter().map(|c| Value::from(c.to_hex())).collect()),
    );
    Value::Object(out)
}

fn diff(path: &str, want: &Value, got: &Value, out: &mut Vec<String>) {
    match (want, got) {
        (Value::Object(w), Value::Object(g)) => {
            for (k, wv) in w {
                match g.get(k) {
                    Some(gv) => diff(&format!("{path}.{k}"), wv, gv, out),
                    None => out.push(format!("{path}.{k}: missing")),
                }
            }
            for k in g.keys().filter(|k| !w.contains_key(*k)) {
                out.push(format!("{path}.{k}: extra"));
            }
        }
        _ if want != got => out.push(format!("{path}: want {want} got {got}")),
        _ => {}
    }
}

#[test]
fn every_case_derives_exactly() {
    let v = vectors();
    let p = Params::house();
    let mut failures = Vec::new();
    for case in v["cases"].as_array().unwrap() {
        let id = case["id"].as_str().unwrap();
        let got = as_json(&derive(&input(&case["input"]), &p));
        diff(id, &case["expect"], &got, &mut failures);
    }
    assert!(
        failures.is_empty(),
        "{} differences:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn the_contrast_more_solver_reproduces_the_house_variant() {
    use phux_design::tokens::TERMINAL;
    let p = Params::house();
    for (mode, terminal) in [(Mode::Dark, TERMINAL.dark), (Mode::Light, TERMINAL.light)] {
        let mut ansi = [Rgb8::BLACK; 16];
        for (slot, c) in ansi.iter_mut().zip(terminal.ansi) {
            *slot = c.rgb();
        }
        let got = contrast_more(&Roles::house(mode), &ansi, &p);
        let mut failures = Vec::new();
        diff(
            mode.key(),
            &as_json(&Roles::house_contrast_more(mode)),
            &as_json(&got),
            &mut failures,
        );
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}

fn tone_input(v: &Value) -> ToneInput {
    let agent = v["agent"].as_str().map(|a| match a {
        "Working" => Agent::Working,
        "Blocked" => Agent::Blocked,
        "Idle" => Agent::Idle,
        "Done" => Agent::Done,
        _ => Agent::Unknown,
    });
    ToneInput {
        agent,
        attention: v["attention"].as_bool().unwrap_or(false),
        asked: v["asked"].as_bool().unwrap_or(false),
        exit: v["exit"].as_i64().map(|e| i32::try_from(e).unwrap()),
        link: v["link"]
            .as_str()
            .map(|l| if l == "lost" { Link::Lost } else { Link::Ok }),
        crashed: v["crashed"].as_bool().unwrap_or(false),
        seen: v["seen"].as_bool().unwrap_or(false),
    }
}

#[test]
fn tone_of_and_plurals_match() {
    let v = vectors();
    for case in v["tone_of"].as_array().unwrap() {
        assert_eq!(
            tone_of(&tone_input(&case["input"])).key(),
            case["expect"],
            "{case}"
        );
    }
    for case in v["plural"].as_array().unwrap() {
        let got = plural(
            case["key"].as_str().unwrap(),
            case["count"].as_u64().unwrap(),
        );
        assert_eq!(got.as_deref(), case["expect"].as_str(), "{case}");
    }
}
