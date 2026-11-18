#!/usr/bin/env python3
"""Real phux + Git smoke test. Starts and stops only private servers."""

import argparse
from contextlib import contextmanager
import json
import os
from pathlib import Path
import shlex
import shutil
import socket
import subprocess
import sys
import tempfile
import time


HERE = Path(__file__).resolve().parent


def run(argv, env=None, ok=True):
    proc = subprocess.run(
        argv, text=True, stdin=subprocess.DEVNULL, capture_output=True, timeout=30, env=env,
    )
    assert (proc.returncode == 0) == ok, (argv, proc.returncode, proc.stdout, proc.stderr)
    return proc.stdout if ok or proc.stdout else proc.stderr


def until(read, matches):
    deadline = time.monotonic() + 15
    while True:
        value = read()
        if matches(value):
            return value
        assert time.monotonic() < deadline, value
        time.sleep(0.05)


def accepting(path):
    with socket.socket(socket.AF_UNIX) as client:
        try:
            client.connect(str(path))
            return True
        except (FileNotFoundError, ConnectionRefusedError):
            return False


@contextmanager
def server(root, phux):
    # Do not inherit config, remote bindings, agents, socket, or login-shell startup.
    env = {"PATH": os.environ["PATH"], "SHELL": "/bin/sh", "LANG": "C",
           "PYTHONDONTWRITEBYTECODE": "1"}
    for key in ("HOME", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_RUNTIME_DIR",
                "XDG_CACHE_HOME", "XDG_DATA_HOME"):
        path = root / key
        path.mkdir(parents=True, exist_ok=True)
        env[key] = str(path)
    route = root / "p.sock"
    cli = [phux, "--socket", str(route)]
    with (root / "server.log").open("wb") as log:
        child = subprocess.Popen(
            [*cli, "server", "--session", "work", "--exit-after-idle", "90"],
            env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=log,
        )
        try:
            until(lambda: accepting(route), bool)
            yield route, cli, env
        except Exception:
            print((root / "server.log").read_text(errors="replace"), file=sys.stderr)
            raise
        finally:
            if child.poll() is None:
                child.terminate()
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait(timeout=5)


def read(path):
    return json.loads(path.read_text())


def no_handle(value):
    # No nested live @N anywhere, including inside the stored receipt.
    if isinstance(value, dict):
        return "terminal_id" not in value and all(no_handle(item) for item in value.values())
    if isinstance(value, list):
        return all(no_handle(item) for item in value)
    return True


def lost_reply_proxy(root, phux, effect):
    proxy = root / ("drop-" + effect)
    match = {
        "create": '[ "$4" = create ] || { [ "$4" = call ] && [ "$5" = terminal.create ]; }',
        "answer": '[ "$3 $4" = "agent respond-question" ]',
        "observe": '[ "$3 $4" = "resource show" ] || [ "$4 $5" = "call resource.show" ]',
    }[effect]
    proxy.write_text(
        "#!/bin/sh\n"
        f"if {match}; then\n"
        f"  {shlex.quote(phux)} \"$@\" >/dev/null || exit \"$?\"\n"
        "  exit 75\n"
        "fi\n"
        f"exec {shlex.quote(phux)} \"$@\"\n"
    )
    proxy.chmod(0o700)
    return str(proxy)


class Proof:
    def __init__(self, root, phux, route, cli, env, legacy):
        self.root, self.phux, self.cli, self.env = root, phux, cli, env
        self.launcher = [sys.executable, str(HERE / "worktree_launcher.py"), "--socket", str(route)]
        if legacy:
            self.launcher.append("--legacy-cli")
        self.repo = root / "repo"
        self.repo.mkdir()
        git = ["git", "-C", str(self.repo)]
        run([*git, "init", "-q"], env)
        run([*git, "-c", "user.email=proof@example.invalid", "-c", "user.name=Proof",
             "commit", "--allow-empty", "-qm", "base"], env)
        inventory = json.loads(run([*cli, "ls", "--json"], env))
        self.session = next(row["key"] for row in inventory["sessions"] if row["name"] == "work")

    def invoke(self, verb, name, *args, executable=None, ok=True):
        cmd = [*self.launcher, "--phux", executable or self.phux, verb,
               str(self.root / f"{name}.json"), *args]
        return json.loads(run(cmd, self.env, ok=ok))

    def launch(self, name, *args, executable=None, ok=True):
        worktree = self.root / f"checkout {name}"
        evidence = self.root / f"{name}.starts"
        command = 'pwd -P >> "$1"; exit 7'
        if name == "anchor":
            command = 'if [ -f "$1" ]; then pwd -P >> "$1"; exec /bin/sleep 30; fi; ' + command
        return self.invoke(
            "launch", name, "--repo", str(self.repo), "--worktree", str(worktree),
            "--base", "HEAD", "--branch", name, "--session", self.session, *args,
            "--", "/bin/sh", "-c", command, "proof", str(evidence),
            executable=executable, ok=ok,
        )

    def committed(self, name):
        return until(
            lambda: self.invoke("recover", name),
            lambda value: (value["current_execution"] or {}).get("state") == "completed",
        )

    def starts(self, name, count):
        logs = (self.root / f"{name}.starts").read_text().splitlines()
        assert logs == [str((self.root / f"checkout {name}").resolve())] * count, logs

    def lost_reply(self):
        error = self.launch("anchor", executable=lost_reply_proxy(self.root, self.phux, "create"), ok=False)
        assert error["error"]["code"] == "unreadable_reply", error
        path = self.root / "anchor.json"
        assert "receipt" not in read(path)
        recovered = self.committed("anchor")
        assert recovered["current_execution"]["outcome"] == {"type": "exited", "code": 7}
        assert recovered["original_execution_matches"], recovered
        journal = read(path)
        assert no_handle(journal), journal
        assert recovered["resource"] == journal["receipt"]["resource"]
        self.starts("anchor", 1)
        duplicate = self.launch("anchor", ok=False)
        assert duplicate["error"]["code"] == "journal_exists", duplicate
        replay = self.invoke("replan-stale", "anchor", ok=False)
        assert replay["error"]["code"] == "replan_refused", replay
        self.starts("anchor", 1)
        return recovered

    def stale_anchor(self, anchor):
        path = self.root / "anchor.json"
        resource = "^" + anchor["resource"]
        run([*self.cli, "relaunch", "--json", resource], self.env)
        replacement = until(
            lambda: self.invoke("recover", "anchor"),
            lambda value: (value["current_execution"] or {}).get("state") == "running",
        )
        assert not replacement["original_execution_matches"], replacement
        self.starts("anchor", 2)
        refused = self.launch("split", "--beside", str(path), ok=False)
        assert refused["error"]["code"] == "stale", refused
        assert not (self.root / "split.starts").exists()

        old = self.root / "split.json"
        original = read(old)
        assert original["error"]["outcome"] == "refused", original
        missing = self.invoke("recover", "split", ok=False)
        assert (missing["error"]["code"], missing["error"]["outcome"]) == (
            "receipt_not_retained", "unknown"
        ), missing
        assert not (self.root / "split.starts").exists()
        result = self.invoke("replan-stale", "split")
        assert result["placement"]["arrangement"] == anchor["placement"]["arrangement"]
        assert result["placement"]["entry"] != anchor["placement"]["entry"]
        replanned = read(old.with_name("split.json.replan"))
        assert read(old) == original, "new intent must not rewrite historical journal"
        assert replanned["request"]["idempotency_key"] != original["request"]["idempotency_key"]
        assert replanned["request"]["placement"]["activation"] == replacement["current_execution"]["activation"]
        assert no_handle(replanned), replanned
        until(lambda: (self.root / "split.starts").exists(), bool)
        self.starts("split", 1)
        self.invoke("replan-stale", "split", ok=False)
        self.starts("split", 1)

    def lost_observation(self):
        proxy = lost_reply_proxy(self.root, self.phux, "observe")
        result = self.launch("observe", executable=proxy, ok=False)
        assert result["error"]["outcome"] == "committed", result
        saved = read(self.root / "observe.json")
        assert no_handle(saved), saved
        assert self.committed("observe")["resource"] == saved["receipt"]["resource"]
        self.starts("observe", 1)

    def wrong_authority(self, legacy):
        with server(self.root / "other", self.phux) as (route, _, env):
            cmd = [sys.executable, str(HERE / "worktree_launcher.py"), "--socket", str(route),
                   "--phux", self.phux]
            if legacy:
                cmd.append("--legacy-cli")
            error = json.loads(run([*cmd, "recover", str(self.root / "anchor.json")], env, ok=False))
            assert error["error"]["code"] == "authority_mismatch", error


def application_profile(env, manifest):
    config = Path(env["XDG_CONFIG_HOME"]) / "phux"
    config.mkdir(parents=True, exist_ok=True)
    config.joinpath("config.toml").write_text(
        f"[[plugins]]\nmanifest = {json.dumps(str(manifest))}\nenabled = true\n"
    )
    manifest.write_text(
        'id = "example.composition"\nname = "Composition proof"\n'
        'version = "1.0.0"\nmin_phux_version = "0.0.2"\n'
    )


def review_fixture(proof, route, legacy):
    anchor = read(proof.root / "anchor.json")
    receipt = anchor["receipt"]
    target = {"authority": anchor["authority"], "session": proof.session,
              "resource": receipt["resource"], **receipt["execution"]}
    cwd = anchor["request"]["cwd"]
    Path(cwd, "change.txt").write_text("A change owned by this checkout\n")
    run(["git", "-C", cwd, "add", "change.txt"], proof.env)
    manifest = proof.root / "phux-plugin.toml"
    application_profile(proof.env, manifest)
    argv = [sys.executable, str(HERE / "review_action.py"), "--phux", proof.phux,
            "--socket", str(route), str(proof.root / "anchor.json")]
    if legacy:
        argv.append("--legacy-cli")
    with manifest.open("a") as file:
        file.write('\n[[actions]]\nid = "review"\ntitle = "Review checkout"\n'
                   f"command = {json.dumps(argv)}\n")
    cmd = [*proof.cli, "config", "run", "example.composition", "review", "--json",
           "--target-json", json.dumps(target), "--timeout", "5"]
    receipt = json.loads(run(cmd, proof.env))
    result = json.loads(receipt["stdout"])
    assert result["target"] == target and result["invocation_source"] == "cli", result
    assert not result["selection_observed"], result
    assert "change.txt" in result["tracked_diffstat"], result
    return cmd


def question_proof(root, phux):
    root.mkdir()
    with server(root, phux) as (route, cli, env):
        manifest = root / "phux-plugin.toml"
        ledger = root / "provider.jsonl"
        application_profile(env, manifest)
        integrations = root / "integrations"
        integrations.mkdir()
        peer = HERE.parents[1] / "crates/phux/tests/fixtures/acp_peer.py"
        command = json.dumps([sys.executable, str(peer), str(ledger)])
        (integrations / "peer.toml").write_text(
            "id = 'peer'\n[acp]\nprovider = 'peer'\n"
            f"command = {command}\nrequired_executables = ['python3']\n"
        )
        launched = json.loads(run([*cli, "launch", "--acp", "peer", "--session", "work", "--json"], env))
        handle, activation = launched["agent_session_id"], launched["activation"]
        try:
            until(lambda: peer_views(json.loads(run([*cli, "agent", "log", handle, "--json"], env))),
                  lambda views: bool(views and views[-1]["connected"]))
            run([*cli, "agent", "native-command", handle, "--activation", activation, "--operation-id",
                 "90", "--intent", '{"type":"prompt","text":"permission"}', "--json"], env)
            typed_questions(root, phux, route, cli, env, handle, ledger)
        finally:
            run([*cli, "kill", "--yes", handle], env)


def peer_views(document):
    updates = [row["data"].get("update", {}) for row in document["records"]]
    return [update["view"] for update in updates if update.get("type") == "view"]


def delivered_answers(ledger):
    # The peer may be appending its last row during the read.
    complete, _, _ = ledger.read_text().rpartition("\n")
    rows = [json.loads(line) for line in complete.splitlines()]
    return [
        row["result"]["outcome"]["optionId"] for row in rows
        if "optionId" in row.get("result", {}).get("outcome", {})
    ]


def typed_questions(root, phux, route, cli, env, handle, ledger):
    current = until(lambda: json.loads(run([*cli, "agent", "questions", handle], env)),
                    lambda value: bool(value["questions"]))
    exact = current["authority"] + "/^" + current["resource"]
    reference = current["questions"][0]["reference"]
    path, journal = root / "question.json", root / "answer.json"
    path.write_text(json.dumps(reference))
    cmd = [sys.executable, str(HERE / "question_inbox.py"), "--socket", str(route)]
    exact_cmd = [*cmd, "--phux", phux]
    reconnected = json.loads(run([*exact_cmd, "list", exact], env))
    assert reconnected == [current], reconnected
    answer = ["answer", str(journal), "--reference", str(path), "--response-json",
              '{"option_id":"deny-once"}']
    proxy = lost_reply_proxy(root, phux, "answer")
    failure = json.loads(run([*cmd, "--phux", proxy, *answer], env, ok=False))
    assert failure["error"]["code"] == "unreadable_reply", failure
    receipt = json.loads(run([*exact_cmd, "recover", str(journal)], env))
    assert receipt["operation_id"] == read(journal)["operation_id"], receipt
    assert receipt["disposition"] == "queued", receipt
    until(lambda: json.loads(run([*exact_cmd, "list", exact], env))[0]["questions"],
          lambda questions: not questions)
    stale = json.loads(run([*exact_cmd, *answer], env, ok=False))
    assert stale["error"]["code"] == "stale", stale
    assert until(lambda: delivered_answers(ledger), bool) == ["deny-once"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--phux", required=True, help="binary to exercise")
    parser.add_argument("--legacy-cli", action="store_true")
    parser.add_argument("--review", action="store_true", help="also exercise configured action context")
    parser.add_argument("--questions", action="store_true", help="also exercise a real ACP callback")
    args = parser.parse_args()
    phux = str(Path(shutil.which(args.phux) or args.phux).resolve(strict=True))
    # Keep the Unix socket below macOS's short sockaddr_un limit.
    with tempfile.TemporaryDirectory(prefix="phux-proof-", dir="/tmp") as directory:
        root = Path(directory).resolve()
        with server(root / "server", phux) as (route, cli, env):
            proof = Proof(root, phux, route, cli, env, args.legacy_cli)
            anchor = proof.lost_reply()
            if args.review:
                review = review_fixture(proof, route, args.legacy_cli)
            proof.stale_anchor(anchor)
            if args.review:
                stale = json.loads(run(review, env, ok=False))
                assert json.loads(stale["stderr"])["error"]["code"] == "stale", stale
                print("review composition: exact action and stale execution refusal passed")
            proof.lost_observation()
            proof.wrong_authority(args.legacy_cli)
        with server(root / "server", phux) as (_, _, _):
            restarted = proof.invoke("recover", "anchor")
            assert restarted["resource"] == anchor["resource"], restarted
            assert not restarted["original_execution_matches"], restarted
        if args.questions:
            question_proof(root / "questions", phux)
            print("question composition: exact callback, lost reply, and stale question passed")
    print("worktree composition: lost replies, stale placement, and authority isolation passed")


if __name__ == "__main__":
    main()
