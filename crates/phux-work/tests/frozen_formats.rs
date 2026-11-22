//! `operation.result_json` rows, read back when a client retries an
//! operation. See `phux_frozen` for what a check proves.

/// What an objective operation stored, in every state: `ObjectiveState` has
/// no fallback, so a renamed state fails the retry that reads it.
#[test]
fn objectives() {
    phux_frozen::json::<Vec<phux_work::Objective>>("objectives").check();
}

/// What a mail send stored.
#[test]
fn mail() {
    phux_frozen::json::<phux_work::Mail>("mail").check();
}
