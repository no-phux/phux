"""Serial controlled-agent acceptance using Probe's actual Cockpit input path.

The producer runs inside a Launcher-owned PTY; this driver spawns no processes.
Receipts and widget text stay in the private work directory or in memory. These
fixture-only widget reads do not extend diagnostic body/provenance guarantees.
"""

import json
import re
import secrets
import shlex
import sys

from .input_roundtrip import ROOT, find_widget, require, resource_inventory, wait_for

PROVIDER = "cockpit-proof"
PHASES = ("opened", "blocked-initial", "blocked", "done", "closed")
REASONS = {
    "blocked-initial": "Preparing terminal intervention proof",
    "blocked": "Enter a decimal proof value in this terminal",
    "done": "Terminal intervention completed",
}
ATTENTION = "\u25cf"


def rendered_id(resource):
    # ts_agents.identity uses the local routing kind (0), not ResourceKind.
    return f"phux:0:{resource[1:]}@"


def read_receipts(path):
    try:
        with path.open("rb") as stream:
            raw = stream.read(16385)
    except FileNotFoundError:
        return []
    require(len(raw) <= 16384, "agent receipt exceeded fixture budget")
    # A read may race the producer's next write. Only complete JSONL records
    # count; malformed completed lines are a failure, not a retry.
    records = [json.loads(line) for line in raw.split(b"\n")[:-1]]
    require([r["phase"] for r in records] == list(PHASES[:len(records)]), "invalid agent receipt order")
    return records


def stamped_record(record, resource, previous=None):
    require(record["resource"] == resource, "agent receipt resource changed")
    for field in ("seq", "ts_ms"):
        require(type(record[field]) is int and 0 < record[field] <= 2**64 - 1,
                "invalid coordinator record stamp")
    if previous is not None:
        require(record["seq"] > previous["seq"], "agent sequence did not advance")
        require(record["ts_ms"] >= previous["ts_ms"], "agent timestamp regressed")
    expected_type = "stop" if record["phase"] == "done" else "ask"
    require(record["type"] == expected_type, "unexpected state-bearing record type")
    return record


def inspection_matches(widgets, view, resource, parent, native_id, record):
    identities = (rendered_id(resource), rendered_id(parent), native_id)
    if not all(find_widget(widgets, "text", value, view) for value in identities):
        return False
    matches = [w for w in widgets if evidence_widget(w, view, record)]
    require(len(matches) <= 1, "ambiguous agent evidence widget")
    return bool(matches)


def evidence_widget(widget, view, record):
    if (widget["view"], widget["role"]) != (view, "text"):
        return False
    state = "done" if record["phase"] == "done" else "blocked"
    evidence = (
        rf"Provider: {PROVIDER}\nCatalog: (unknown|working|blocked|done|gone); records: {state}\n"
        rf"Latest record: {record['type']}\nSequence: {record['seq']}\n"
        rf"Coordinator-stamped record time \(ts_ms\): {record['ts_ms']}\n"
        rf"Reason: {re.escape(REASONS[record['phase']])}"
    )
    return re.fullmatch(evidence, widget["name"]) is not None


class AgentAcceptance:
    def __init__(self, probe, parent, sibling, view):
        self.probe, self.parent, self.sibling, self.view = probe, parent, sibling, view
        self.native_id = "rt-agent-" + secrets.token_hex(8)
        self.receipt_path = probe.launcher.work / "agent-receipt.jsonl"
        self.observed = []
        self.resource = None
        self.baseline = self.catalog()
        self.expected = dict(self.baseline)
        self.terminals = probe.inventory()
        require(set(self.baseline) == self.terminals, "agent fixture requires a terminal-only baseline")

    def catalog(self):
        return resource_inventory(self.probe.server("ls"))

    def unchanged(self):
        require(self.catalog() == self.expected, "agent workflow changed unrelated resource inventory")

    def widgets(self):
        self.unchanged()
        return self.probe.snapshot()[0]

    def receipts(self):
        self.probe.launcher.check_app()
        records = read_receipts(self.receipt_path)
        require(records[:len(self.observed)] == self.observed, "agent receipts were replaced or rewritten")
        self.observed = records
        return records

    def await_receipt(self, phase):
        index = PHASES.index(phase)

        def completed():
            records = self.receipts()
            return records[index] if len(records) > index else None

        return wait_for(completed, "producer receipt: " + phase)

    def held_at(self, phase):
        require(len(self.receipts()) == PHASES.index(phase) + 1, "producer advanced without terminal intervention")
        self.unchanged()

    def focus(self, target):
        self.probe.click(self.probe.owner(target, self.view))
        wait_for(lambda: self.probe.owner(target, self.view)["focused"] == "true", "controlled terminal focus")

    def submit(self, text, before_enter=None):
        require(self.probe.owner(self.parent, self.view)["focused"] == "true",
                "agent parent not focused before widget-key")
        self.probe.input_text(self.parent, self.view, text, "key")
        self.unchanged()
        if before_enter is not None:
            before_enter()
        self.probe.key(self.parent, self.view, "enter")

    def launch(self):
        self.probe.evidence["phase"] = "agent-birth"
        self.probe.select_group_target(self.sibling, [self.parent, self.sibling])
        require(self.probe.owner(self.sibling, self.view)["focused"] == "true", "sibling must be focused initially")
        require(self.probe.owner(self.parent, self.view)["focused"] == "false", "parent unexpectedly focused initially")
        require(not self.receipt_path.exists(), "agent receipt path already exists")
        # Matrix starts with top tabs. Use the real command to expose the rail's
        # exact-parent row and attention marker, then restore top after retirement.
        self.probe.automate("native-command", "tabs.toggle-placement", self.view)
        self.focus(self.parent)
        fixture = ROOT / "scripts/agent-attention-proof.py"
        require(fixture.is_file(), "integrated agent producer fixture is missing")
        command = shlex.join([sys.executable, str(fixture), "--phux", str(self.probe.launcher.args.phux),
                              "--socket", self.probe.launcher.env["PHUX_SOCKET"],
                              "--run-id", self.native_id, "--receipt", str(self.receipt_path)])
        self.submit(command)
        self.focus(self.sibling)
        self.accept_birth(self.await_receipt("opened"))

    def accept_birth(self, opened):
        require(opened["parent"] == self.parent, "agent born under wrong terminal")
        require(opened["native_id"] == self.native_id, "agent native identity mismatch")
        require(opened["provider"] == PROVIDER, "agent provider mismatch")
        self.resource = opened["resource"]
        require(self.resource not in self.baseline, "agent identity predates attach")
        self.expected[self.resource] = {"kind": "agent_session", "parent": self.parent}
        self.unchanged()
        self.record_result("agent-birth")

    def rail(self, state, attention):
        widgets = self.widgets()
        row = self.rail_row(widgets)
        state_widget = find_widget(widgets, "text", state, self.view)
        mark = find_widget(widgets, "text", ATTENTION, self.view)
        return bool(row and state_widget) and bool(mark) == attention

    def rail_row(self, widgets):
        label = f"{PROVIDER} / {rendered_id(self.resource)} under {rendered_id(self.parent)}"
        return find_widget(widgets, "button", label, self.view)

    def inspect(self, record):
        self.probe.click(self.probe.widget("button", "Agents 1", self.view))
        wait_for(lambda: inspection_matches(self.widgets(), self.view, self.resource,
                                            self.parent, self.native_id, record), "exact agent inspection evidence")

    def jump(self):
        self.probe.click(self.probe.widget("button", "Jump to parent", self.view))
        wait_for(lambda: self.probe.owner(self.parent, self.view)["focused"] == "true", "Jump focused exact parent")
        require(self.probe.owner(self.sibling, self.view)["focused"] == "false", "Jump retained sibling focus")
        require(self.probe.widget("button", "Close agent inspector", self.view) is None, "Jump left inspector open")

    def blocked(self, phase, previous=None):
        self.probe.evidence["phase"] = "agent-" + phase
        record = stamped_record(self.await_receipt(phase), self.resource, previous)
        require(record["ts_ms"] > self.probe.launcher.app["started_unix"] * 1000,
                "agent evidence predates Cockpit attach")
        self.held_at(phase)
        wait_for(lambda: self.rail("blocked", True), "blocked exact-parent rail row")
        if previous is None:
            require(self.probe.owner(self.sibling, self.view)["focused"] == "true", "ask did not arrive with sibling focused")
        self.inspect(record)
        self.held_at(phase)
        self.jump()
        self.held_at(phase)
        wait_for(lambda: self.rail("blocked", True), "Jump preserved blocked attention")
        self.record_result("agent-" + phase, record)
        return record

    def readonly_roundtrip(self, record):
        # Reopen after Jump: neither navigation nor read-only inspection may
        # replace the blocked evidence or clear its visible attention marker.
        self.probe.evidence["phase"] = "agent-readonly-jump"
        self.inspect(record)
        self.probe.click(self.probe.widget("button", "Close agent inspector", self.view))
        self.held_at(record["phase"])
        wait_for(lambda: self.rail("blocked", True), "read-only inspection preserved attention")
        self.focus(self.sibling)
        self.inspect(record)
        self.jump()
        self.held_at(record["phase"])
        wait_for(lambda: self.rail("blocked", True), "repeated Jump preserved attention")
        self.record_result("agent-readonly-jump", record)

    def intervention(self, previous):
        self.probe.evidence["phase"] = "agent-intervention"
        answer = str(secrets.randbelow(800000000) + 100000000)
        result = str(int(answer) + 451)
        require(result not in answer, "invalid computed agent fixture")
        self.probe.absent(self.terminals, result, "agent computed result already present before input")
        self.submit(answer, lambda: self.probe.absent(self.terminals, result, "agent result appeared before Enter"))
        done = stamped_record(self.await_receipt("done"), self.resource, previous)
        require(done["computed_result"] == result, "producer computed a different result")
        wait_for(lambda: self.probe.present(self.parent, result), "agent computed result in exact parent PTY")
        self.probe.absent(self.terminals - {self.parent}, result)
        self.held_at("done")
        wait_for(lambda: self.rail("done", False), "producer reconciled state and attention")
        self.inspect(done)
        self.jump()
        self.held_at("done")
        self.record_result("agent-intervention", done)

    def retired(self):
        actual = self.catalog()
        if self.resource in actual:
            require(actual == self.expected, "agent retirement changed unrelated resources")
            return False
        require(actual == self.baseline, "agent retirement changed unrelated resources")
        return True

    def retire(self):
        self.probe.evidence["phase"] = "agent-retirement"
        self.submit("close")
        closed = self.await_receipt("closed")
        require((closed["resource"], closed["parent"]) == (self.resource, self.parent), "retired wrong agent identity")
        wait_for(self.retired, "only intended agent resource retired")
        self.expected = dict(self.baseline)
        wait_for(lambda: self.probe.widget("button", "Agents 0", self.view), "agent row count reconciled")
        require(self.rail_row(self.widgets()) is None, "retired agent rail row survived")
        self.probe.click(self.probe.widget("button", "Agents 0", self.view))
        wait_for(lambda: find_widget(self.widgets(), "text", "No agent resources in the attached catalog", self.view),
                 "empty agent inspector after retirement")
        require(find_widget(self.widgets(), "text", rendered_id(self.resource), self.view) is None,
                "retired agent inspection row survived")
        self.probe.click(self.probe.widget("button", "Close agent inspector", self.view))
        self.probe.automate("native-command", "tabs.toggle-placement", self.view)
        self.held_at("closed")
        self.record_result("agent-retirement")

    def record_result(self, phase, record=None):
        metadata = {} if record is None else {key: record[key] for key in ("seq", "ts_ms", "type")}
        self.probe.evidence["results"].append({
            "phase": phase, "status": "PASS", "resource": self.resource, "parent": self.parent,
            "native_id": self.native_id, "provider": PROVIDER, "view": self.view,
            "publisher_pid": self.probe.launcher.app["pid"], "record": metadata,
            "inventory_count": len(self.expected), "other_ptys_checked": len(self.terminals) - 1,
        })

    def run(self):
        self.launch()
        first = self.blocked("blocked-initial")
        self.readonly_roundtrip(first)
        self.submit("inspect")
        second = self.blocked("blocked", first)
        self.intervention(second)
        self.retire()
