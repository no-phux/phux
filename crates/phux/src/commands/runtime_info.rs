//! Read-only runtime composition contract. Never loads config or dials a server.
use std::process::ExitCode;

fn document() -> serde_json::Value {
    let wire = phux_protocol::PROTOCOL_VERSION;
    serde_json::json!({
        "schema_version": 1,
        "binary": "phux",
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": { "major": wire.major, "minor": wire.minor, "patch": wire.patch },
        "capabilities": ["server-ensure-v1", "structured-spawn-v1", "host-enroll-v1"]
    })
}

pub(crate) fn run(json: bool) -> ExitCode {
    if json {
        outln!("{}", document());
    } else {
        outln!("Phux {} (runtime-info schema 1)", env!("CARGO_PKG_VERSION"));
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    #[test]
    fn runtime_contract_is_versioned_and_does_not_depend_on_environment() {
        let doc = super::document();
        assert_eq!(doc["schema_version"], 1);
        assert_eq!(doc["binary"], "phux");
        assert_eq!(
            doc["protocol"]["major"],
            phux_protocol::PROTOCOL_VERSION.major
        );
        assert_eq!(
            doc["protocol"]["minor"],
            phux_protocol::PROTOCOL_VERSION.minor
        );
        assert_eq!(doc["capabilities"][0], "server-ensure-v1");
    }
}
