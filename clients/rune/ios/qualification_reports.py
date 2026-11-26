"""Credential-free native qualification report contracts."""
import uuid

SESSION = "rune-ios-native"
INPUT_CONTRACTS = ("field-editing", "rejected-edit", "composition-revision", "raw-composition",
                   "raw-replacement", "hardware-routing", "directional-selection", "dismissal",
                   "pairing-import", "pairing-confirmation", "pairing-save-retry", "pairing-cancelled-identity",
                   "tailnet-account", "tailnet-errors", "tailnet-import", "native-control-stability",
                   "native-control-teardown", "earlier-app-scenes",
                   "earlier-app-hosts")


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


def _require_fields(value, expected):
    for field, wanted in expected.items():
        if type(value.get(field)) is not type(wanted) or value[field] != wanted:
            raise ValueError(f"invalid qualification tree {field}: {value.get(field)!r}")


def _validate_tree_binding(tree, report, marker):
    frames = report.get("frames")
    if type(frames) is not int or frames < 1:
        raise ValueError("qualification tree has no presented frame")
    terminal = report.get("terminal_id")
    if not isinstance(terminal, str) or not terminal.strip():
        raise ValueError("qualification tree has no terminal identity")
    _require_fields(report, {"tree_marker_seen": True, "tree_frames": frames})
    metadata = tree.get("qualification")
    if not isinstance(metadata, dict):
        raise ValueError("native view tree has no qualification metadata")
    _require_fields(metadata, {"schema": 1, "marker": marker, "frames": frames,
                               "terminal_id": terminal})


def _tree_nodes(tree):
    """Walk only accessibility children, never arbitrary metadata objects."""
    if not isinstance(tree, dict) or not isinstance(tree.get("role"), str):
        raise ValueError("invalid native accessibility tree node")
    yield tree
    children = tree.get("children", [])
    if not isinstance(children, list):
        raise ValueError("invalid native accessibility tree children")
    for child in children:
        yield from _tree_nodes(child)


def _grid_has_marker(grid, marker):
    if not isinstance(marker, str) or not marker:
        return False
    labels = [node["name"] for node in list(_tree_nodes(grid))[1:]
              if node["role"] == "label" and isinstance(node.get("name"), str)]
    # Closed delimiters distinguish phases even across row/style boundaries;
    # bare identifier boundaries cannot distinguish a wrap from a new line.
    rendered = "".join(labels).replace("\n", "")
    return "[[" + marker + "]]" in rendered


def validate_tree(tree, report, marker, session=SESSION):
    nodes = list(_tree_nodes(tree))
    _validate_tree_binding(tree, report, marker)
    if not any(isinstance(node.get("name"), str) and session in node["name"] for node in nodes):
        raise ValueError("native view tree does not show the qualification session")
    grids = [node for node in nodes if node.get("key") == "grid-" + report["terminal_id"]]
    if len(grids) != 1:
        raise ValueError("native view tree does not identify the selected terminal grid")
    if not _grid_has_marker(grids[0], marker):
        raise ValueError("native selected terminal grid does not show the qualification marker")
