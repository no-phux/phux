use std::collections::BTreeSet;

/// Every `u8` wire-tag const here is declared through `wire_tags!`
/// (E0119 on reuse), and `TYPE_` / `COMMAND_TAG_` consts take their value
/// from [`FrameType`] / [`CommandTag`] (E0081 on reuse).
#[test]
fn wire_tag_consts_are_uniqueness_checked() {
    // Only the const-declaration half of the file is scanned; the test
    // module itself would otherwise match its own patterns.
    let src = include_str!("mod.rs");
    let src = src.split("#[cfg(test)]").next().unwrap_or(src);
    // The tag families declared in the sibling tag modules are scanned too.
    let src: &str = &[
        src,
        include_str!("tags.rs"),
        include_str!("command_tags.rs"),
    ]
    .concat();

    let mut reserved: BTreeSet<&str> = BTreeSet::new();
    let mut rest = src;
    while let Some(start) = rest.find("wire_tags!") {
        let after = &rest[start..];
        let Some(brace) = after.find('{') else {
            panic!("wire_tags! without a body");
        };
        let mut depth = 0_i32;
        let mut end = None;
        for (i, ch) in after[brace..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(brace + i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let end = end.expect("unterminated wire_tags! body");
        for name in after[brace..=end]
            .split(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
        {
            if name.contains('_') && name.chars().any(|c| c.is_ascii_uppercase()) {
                reserved.insert(name);
            }
        }
        rest = &after[end + 1..];
    }
    assert!(
        !reserved.is_empty(),
        "no wire_tags! families found - the compile-time checks are gone"
    );

    for line in src.lines() {
        let line = line.trim();
        let decl = line
            .strip_prefix("pub const ")
            .or_else(|| line.strip_prefix("pub(crate) const "));
        let Some(decl) = decl else { continue };
        let Some((name, expr)) = decl.split_once(": u8 = ") else {
            continue;
        };
        let expr = expr.trim_end_matches(';').trim();
        if name.starts_with("TYPE_") {
            assert!(
                expr.starts_with("FrameType::"),
                "{name} must take its value from the FrameType enum so a duplicate \
                 discriminant is a compile error, found `{expr}`"
            );
            assert!(
                reserved.contains(name),
                "{name} must be declared through wire_tags! so a reused byte is E0119"
            );
        } else if name.starts_with("COMMAND_TAG_") {
            assert!(
                expr.starts_with("CommandTag::"),
                "{name} must take its value from the CommandTag enum so a duplicate \
                 discriminant is a compile error, found `{expr}`"
            );
            assert!(
                reserved.contains(name),
                "{name} must be declared through wire_tags! so a reused byte is E0119"
            );
        } else if expr.starts_with("0x") || expr.starts_with(|c: char| c.is_ascii_digit()) {
            assert!(
                reserved.contains(name),
                "{name} is a hand-allocated literal wire tag missing from a \
                 wire_tags! family"
            );
        }
    }
}
