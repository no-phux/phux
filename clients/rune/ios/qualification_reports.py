"""Credential-free native qualification report contracts."""
import uuid

SESSION = "rune-ios-native"
INPUT_CONTRACTS = ("field-editing", "rejected-edit", "composition-revision", "raw-composition",
                   "raw-replacement", "hardware-routing", "directional-selection", "dismissal",
                   "pairing-import", "pairing-confirmation", "pairing-save-retry", "pairing-cancelled-identity",
                   "tailnet-account", "tailnet-errors", "tailnet-import")


def validate_report(report, transport="quic", resumed=False):
    if report.get("error"):
        raise ValueError(f"native qualification error: {report['error']}")
    expected = {"schema": 1, "session": SESSION, "connected": True,
                "input_through_native": True, "marker_seen": True, "transport": f"paired-{transport}",
                "onboarding_import": not resumed, "onboarding_confirmed": not resumed,
                "resumed_identity": resumed, "surface_at_scale": True}
    for field, value in expected.items():
        if type(report.get(field)) is not type(value) or report[field] != value:
            raise ValueError(f"invalid qualification {field}: {report.get(field)!r}")
    if type(report.get("frames")) is not int or report["frames"] < 1:
        raise ValueError("qualification drew no frames")
    if not isinstance(report.get("terminal_id"), str) or not report["terminal_id"].strip():
        raise ValueError("qualification has no terminal identity")
    validate_input_contracts(report.get("input_contracts"))
    if not isinstance(report.get("key_id"), str):
        raise ValueError("qualification has no public device-key tag")
    uuid.UUID(report["key_id"])


def validate_input_contracts(contracts):
    if (not isinstance(contracts, list) or not all(isinstance(c, str) for c in contracts)
            or len(set(contracts)) != len(contracts) or not set(INPUT_CONTRACTS).issubset(contracts)):
        raise ValueError("native UIKit input contract groups did not pass")
