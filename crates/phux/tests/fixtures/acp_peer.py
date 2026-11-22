"""Deterministic ACP JSON-RPC peer; no model, terminal emulation or network."""
import json
import os
import sys

log_path = sys.argv[1]
session = "peer-stable-session"
pending = None
cancelled = False
callback = 900
load_supported = len(sys.argv) < 3 or sys.argv[2] != "no-load"


def record(event):
    with open(log_path, "a", encoding="utf8") as output:
        output.write(json.dumps(event) + "\n")


def send(value):
    print(json.dumps({"jsonrpc": "2.0", **value}), flush=True)


def reply(identity, value):
    send({"id": identity, "result": value})


def update(value):
    send({"method": "session/update", "params": {"sessionId": session, "update": value}})


def finish(outcome):
    global pending
    update({"sessionUpdate": "tool_call_update", "toolCallId": "tool-1", "status": "completed", "rawOutput": {"outcome": outcome}})
    update({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "peer result: " + outcome}})
    reply(pending, {"stopReason": "cancelled" if cancelled else "end_turn"})
    pending = None


record({"started": os.getpid(), "private_binding": any(key.startswith("PHUX_NATIVE_") for key in os.environ)})
for line in sys.stdin:
    message = json.loads(line)
    record(message)
    method = message.get("method")
    identity = message.get("id")
    params = message.get("params", {})
    if method == "initialize":
        caps = params.get("clientCapabilities", {})
        assert not caps.get("terminal", False)
        assert not any(caps.get("fs", {}).values())
        reply(identity, {"protocolVersion": 1, "agentCapabilities": {"loadSession": load_supported}, "agentInfo": {"name": "deterministic-peer", "version": "1"}})
    elif method in ("session/new", "session/load"):
        if method == "session/load":
            assert load_supported
            assert params["sessionId"] == session
            update({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "historical assistant"}})
            reply(identity, {})
        else:
            reply(identity, {"sessionId": session})
        send({"id": 800, "method": "fs/read_text_file", "params": {"sessionId": session, "path": "/never-read"}})
        send({"id": 801, "method": "terminal/create", "params": {"sessionId": session, "command": "never-executed"}})
        send({"id": 802, "method": "_unsupported_peer_extension", "params": {}})
    elif method == "session/prompt":
        assert pending is None
        assert params["sessionId"] == session
        pending = identity
        cancelled = False
        text = params["prompt"][0]["text"]
        if text == "disconnect":
            sys.exit(0)
        update({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "streamed "}})
        update({"sessionUpdate": "tool_call", "toolCallId": "tool-1", "title": "inert tool", "kind": "other", "status": "pending", "rawInput": {"command": "never executed"}})
        callback += 1
        send({"id": callback, "method": "session/request_permission", "params": {"sessionId": session, "toolCall": {"toolCallId": "tool-1", "title": "Explicit human choice"}, "options": [{"optionId": "allow-once", "name": "Allow once", "kind": "allow_once"}, {"optionId": "deny-once", "name": "Deny once", "kind": "reject_once"}]}})
    elif method == "session/cancel":
        cancelled = True
    elif identity in (800, 801, 802):
        assert message["error"]["code"] == -32601
    elif identity == callback:
        outcome = message["result"]["outcome"]
        finish(outcome.get("optionId", outcome["outcome"]))
    else:
        send({"id": identity, "error": {"code": -32601, "message": "unsupported"}})
