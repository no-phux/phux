//! The checkout JSON codec and the contract fold.

use super::*;

#[test]
fn requests_are_tagged_by_op() {
    let request = CheckoutRequest::Diff {
        path: "src/main.rs".to_owned(),
        area: CheckoutArea::Staged,
    };
    let bytes = encode_request(&request);
    assert_eq!(
        std::str::from_utf8(&bytes).unwrap(),
        r#"{"op":"diff","path":"src/main.rs","area":"staged"}"#
    );
    assert!(matches!(decode_request(&bytes), Ok(decoded) if decoded == request));
    // A newer op or field is refused, but as unsupported, not malformed.
    assert!(matches!(
        decode_request(br#"{"op":"exec","argv":["rm"]}"#),
        Err(CheckoutDecodeError::Unsupported)
    ));
    assert!(matches!(
        decode_request(br#"{"op":"read","path":"a","max_bytes":1,"extra":1}"#),
        Err(CheckoutDecodeError::Unsupported)
    ));
    for malformed in [
        br#"{"op":"read""#.as_slice(),
        br#"{"op":"read"}"#,
        br#"{"op":"read","path":1,"max_bytes":1}"#,
    ] {
        assert!(matches!(
            decode_request(malformed),
            Err(CheckoutDecodeError::Invalid)
        ));
    }
}

#[test]
fn contract_one_never_sees_a_contract_two_answer() {
    let refusal = |code| CheckoutRefusal {
        code,
        message: "m".to_owned(),
    };
    let applied = CheckoutAnswer::Applied(refusal(CheckoutRefusalCode::Timeout));
    assert_eq!(applied.clone().for_contract(2), applied);
    assert_eq!(
        applied.for_contract(1),
        CheckoutAnswer::Refused(refusal(CheckoutRefusalCode::Timeout))
    );
    let missing = CheckoutAnswer::Applied(refusal(CheckoutRefusalCode::GitUnavailable));
    assert_eq!(
        missing.for_contract(1),
        CheckoutAnswer::Refused(refusal(CheckoutRefusalCode::GitFailed))
    );
}

#[test]
fn answers_tolerate_future_fields_and_values() {
    let decoded =
        decode_answer(br#"{"kind":"refused","code":"quota_exceeded","message":"m","later":true}"#);
    assert!(matches!(
        decoded,
        Ok(CheckoutAnswer::Refused(CheckoutRefusal {
            code: CheckoutRefusalCode::Unknown,
            ..
        }))
    ));
}
