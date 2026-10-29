#[cfg(test)]
mod tests {
    use std::path::Path;

    use phux_client::attach::AttachError;

    use super::skew_handover_lines;
    use crate::commands::upgrade::UpgradeAck;

    fn incompatible() -> AttachError {
        AttachError::IncompatibleProtocol(
            "incompatible protocol: client offered 1.4.0, server requires 0.9.x; \
             update the phux server so protocol major.minor match"
                .to_owned(),
        )
    }

    #[test]
    fn an_incompatible_refusal_prints_the_stop_recovery() {
        let text = skew_handover_lines(
            "0.9.0",
            "1.4.0",
            &Err(incompatible()),
            Path::new("/tmp/old.sock"),
            Some(4242),
        )
        .join("\n");
        assert!(text.contains("`kill 4242`"), "{text}");
        assert!(
            text.contains("`ps -p 4242 -o pid=,ppid=,user=,args=`"),
            "{text}"
        );
        assert!(
            text.contains("phux server --ensure --socket '/tmp/old.sock'`"),
            "{text}"
        );
        assert!(!text.contains("in place"), "{text}");
        assert!(!text.contains("continuing on"), "{text}");
        for forbidden in [
            "stale",
            "phux upgrade",
            "survive",
            "remove",
            "pkill",
            "killall",
        ] {
            assert!(!text.contains(forbidden), "{forbidden} in {text}");
        }

        let unknown = skew_handover_lines(
            "0.9.0",
            "1.4.0",
            &Err(incompatible()),
            Path::new("/tmp/old.sock"),
            None,
        )
        .join("\n");
        assert!(
            unknown.contains("`lsof -nP -- '/tmp/old.sock'`"),
            "{unknown}"
        );
        assert!(!unknown.contains("pkill"), "{unknown}");
        assert!(!unknown.contains("killall"), "{unknown}");
        assert!(!unknown.contains("`kill "), "{unknown}");
    }

    #[test]
    fn an_accepted_handover_is_the_only_in_place_line() {
        let text = skew_handover_lines(
            "0.14.0",
            "0.15.0",
            &Ok(UpgradeAck::Upgrading),
            Path::new("/tmp/s.sock"),
            None,
        )
        .join("\n");
        assert!(text.contains("upgrading it in place"), "{text}");
        assert!(!text.contains("continuing on"), "{text}");
        assert!(!text.contains("pkill"), "{text}");
    }

    #[test]
    fn a_compatible_refusal_keeps_continuing_on() {
        let text = skew_handover_lines(
            "0.14.0",
            "0.15.0",
            &Ok(UpgradeAck::Refused("agent sessions are open".to_owned())),
            Path::new("/tmp/s.sock"),
            None,
        )
        .join("\n");
        assert!(text.contains("continuing on 0.14.0"), "{text}");
        assert!(!text.contains("upgrading it in place"), "{text}");
        assert!(!text.contains("pkill"), "{text}");
    }
}
