/// All credential operations require both verbs at Global, never pane authority.
fn credential_rows() -> Vec<Row> {
    use phux_protocol::credentials::{CredentialOp, CredentialRequest};
    [
        CredentialOp::Next { provider: "desktop".into() },
        CredentialOp::Request(CredentialRequest {
            provider: "desktop".into(),
            references: vec!["op://vault/item/field".into()],
            purpose: "policy matrix".into(),
        }),
        CredentialOp::Complete { id: "a".repeat(32), values: None },
    ].into_iter().map(|op| row(
        command(Command::Credentials { op }),
        &[&["bind,input@global"]],
        &[
            &["bind@global"],
            &["input@global"],
            &["bind,input@host"],
            &["bind,input@terminal:1"],
        ],
    )).collect()
}
