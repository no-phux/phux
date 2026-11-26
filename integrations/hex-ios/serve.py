"""Forward a Hex Shortcut's transcript to one dedicated OpenCode pane."""

import argparse
import hmac
import json
import re
import subprocess
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

MAX_BODY = 32 * 1024
MAX_PROMPT = 4096  # phux agent prompt's text limit
PROMPT_PREFIX = "Voice request: "
QUEUED = "Queued for the phux chief."
UNAVAILABLE = "The phux chief is unavailable. Nothing was sent."
BUSY = "The phux chief is busy or waiting for input. Nothing was sent."
REFUSED = "The phux chief refused the prompt. Nothing was queued."
UNKNOWN = "Delivery unknown. Check the chief before sending again."


def readiness(phux, target):
    try:
        reply = subprocess.run(
            [*phux, "agent", "show", "--json", target],
            check=True,
            capture_output=True,
            timeout=5,
        )
        (agent,) = json.loads(reply.stdout)["agents"]
        return None if agent["state"] in {"idle", "done"} else BUSY
    except (OSError, subprocess.SubprocessError, ValueError, KeyError, TypeError):
        return UNAVAILABLE


def submit(phux, pane_key, text):
    target = "^" + pane_key
    if reason := readiness(phux, target):
        return reason
    try:
        reply = subprocess.run(
            [
                *phux,
                "agent",
                "prompt",
                "--json",
                "--expect-kind",
                "opencode",
                "--",
                target,
                PROMPT_PREFIX + text,
            ],
            check=False,
            capture_output=True,
            timeout=10,
        )
        if reply.returncode == 0:
            return (
                QUEUED if json.loads(reply.stdout)["delivery"] == "acked" else UNKNOWN
            )
        error = json.loads(reply.stderr)["error"]
        if error["code"] in {"no_agent_record", "no_such_target"}:
            return UNAVAILABLE
        if error.get("recovery", {}).get("effect_certainty") == "not_applied":
            return REFUSED
        return UNKNOWN
    except OSError:
        return UNAVAILABLE
    except (subprocess.TimeoutExpired, ValueError, KeyError, TypeError):
        return UNKNOWN


class Handler(BaseHTTPRequestHandler):
    def setup(self):
        self.request.settimeout(5)
        super().setup()

    def log_message(self, *_):
        # The transcript, token, and request data do not belong in service logs.
        pass

    def respond(self, status, message):
        body = message.encode()
        self.send_response(status)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def read_transcript(self):
        lengths = self.headers.get_all("Content-Length", [])
        if self.headers.get("Transfer-Encoding") or len(lengths) != 1:
            raise ValueError("Expected a fixed-length JSON request.")
        length = int(lengths[0])
        if not 0 < length <= MAX_BODY:
            raise ValueError("The request is empty or too large.")
        if self.headers.get_content_type() != "application/json":
            raise ValueError("Expected JSON from Shortcuts.")

        body = self.rfile.read(length)
        if len(body) != length:
            raise ValueError("Incomplete request.")
        payload = json.loads(body)
        text = payload.get("text") if isinstance(payload, dict) else None
        if not isinstance(text, str):
            raise TypeError("Expected a text transcript.")
        # Prompt is one paste plus Enter. Keep dictation line breaks and tabs
        # from turning into terminal keystrokes, and reject other controls.
        text = " ".join(text.split())
        if re.search(r"[\x00-\x1f\x7f-\x9f]", text):
            raise ValueError("The transcript contains control characters.")
        if len((PROMPT_PREFIX + text).encode()) > MAX_PROMPT:
            raise ValueError("The transcript exceeds the prompt limit.")
        return text

    def do_POST(self):
        if self.path != "/":
            self.respond(404, "No such route.")
            return
        authorization = self.headers.get("Authorization", "").encode("latin1")
        if not hmac.compare_digest(authorization, self.server.authorization):
            self.respond(401, "The phone is not authorized.")
            return
        try:
            text = self.read_transcript()
        except (TimeoutError, ValueError, TypeError, RecursionError):
            self.respond(400, "Invalid or oversized transcript. Nothing was sent.")
            return
        if not text:
            self.respond(200, "No transcript. Nothing was sent.")
            return
        # Shortcuts can display delivery outcomes as plain text, without treating a
        # refused or ambiguous phux receipt as a failed HTTP connection.
        self.respond(200, self.server.submit(text))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--phux", type=Path, required=True, help="Absolute path to phux"
    )
    parser.add_argument(
        "--socket", type=Path, help="Optional explicit local phux socket"
    )
    parser.add_argument("--port", type=int, default=18468)
    parser.add_argument(
        "--pane-key", required=True, help="Full stable key of the OpenCode pane"
    )
    parser.add_argument(
        "--token-file", type=Path, required=True, help="Private bearer token file"
    )
    args = parser.parse_args()
    if not args.phux.is_absolute():
        parser.error("--phux must be absolute")
    if not re.fullmatch(r"[a-z2-7]{26}", args.pane_key):
        parser.error("--pane-key must be a full phux resource key")
    token = args.token_file.read_bytes().strip()
    if not re.fullmatch(rb"[A-Za-z0-9_-]{32,}", token):
        parser.error("expected a URL-safe token of at least 32 characters")

    phux = [str(args.phux)]
    if args.socket is not None:
        phux.extend(("--socket", str(args.socket)))
    with HTTPServer(("127.0.0.1", args.port), Handler) as server:
        server.authorization = b"Bearer " + token
        server.submit = lambda text: submit(phux, args.pane_key, text)
        server.serve_forever()


if __name__ == "__main__":
    main()
