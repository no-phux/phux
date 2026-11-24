use super::*;

fn items(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

fn feed(list: &[&str], chunks: &[&[u8]]) -> Step {
    let items = items(list);
    let mut picker = Picker {
        items: &items,
        line: Vec::new(),
    };
    let mut echo = Vec::new();
    for chunk in chunks {
        let step = picker.feed(chunk, &mut echo);
        if step != Step::More {
            return step;
        }
    }
    Step::More
}

#[test]
fn a_typed_answer_picks_by_text_then_by_number() {
    // A client's card types the exact item and Enter, in one read.
    assert_eq!(feed(&["a", "b"], &[b"b\r"]), Step::Picked("b".into()));
    assert_eq!(feed(&["a", "b"], &[b"2", b"\r"]), Step::Picked("b".into()));
    // Exact text wins over a number, so a card's answer is never misread.
    assert_eq!(feed(&["2", "1"], &[b"1\r"]), Step::Picked("1".into()));
    // Backspace drops a whole character before the choice is read.
    assert_eq!(
        feed(&["é", "ab"], &["éx".as_bytes(), b"\x7f\r"]),
        Step::Picked("é".into())
    );
}

#[test]
fn escape_and_ctrl_keys_cancel_but_an_arrow_does_not() {
    assert_eq!(feed(&["a"], &[b"\x1b"]), Step::Cancel);
    assert_eq!(feed(&["a"], &[b"\x03"]), Step::Cancel);
    assert_eq!(feed(&["a"], &[b"\x04"]), Step::Cancel);
    assert_eq!(feed(&["a"], &[b"\x1b[A"]), Step::More);
    // An unknown answer asks again rather than ending.
    assert_eq!(feed(&["a"], &[b"zz\r"]), Step::More);
    assert_eq!(feed(&["a"], &[b"zz\r", b"1\r"]), Step::Picked("a".into()));
}

#[test]
fn the_ask_title_carries_only_what_the_sentinel_grammar_can() {
    let title = ask_title("pick-1", "Which?s=x", &items(&["a|b", "c", "d\x07"]));
    assert_eq!(title, "\x1b]2;phux-ask[pick-1]:Which? s=x?s=c|d\x07");
    let marker = phux_client_core::ask::parse_ask_title(&title[4..title.len() - 1]).unwrap();
    assert_eq!(marker.question, "Which? s=x");
    assert_eq!(marker.suggestions, ["c", "d"]);
}

#[test]
fn items_come_from_arguments_else_nonblank_stdin_lines() {
    let args = items(&["x"]);
    assert_eq!(items_from(args, &b"a\n"[..]).unwrap(), ["x"]);
    assert_eq!(
        items_from(Vec::new(), &b"a\n\n b\n"[..]).unwrap(),
        ["a", " b"]
    );
}
