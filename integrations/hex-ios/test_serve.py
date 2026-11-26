import http.client
import json
import subprocess
import threading
import unittest
from http.server import HTTPServer
from unittest.mock import patch

import serve


class ShortcutRequests(unittest.TestCase):
    def setUp(self):
        self.sent = []
        self.server = HTTPServer(("127.0.0.1", 0), serve.Handler)
        self.server.authorization = b"Bearer test-token"
        self.server.submit = self.submit
        worker = threading.Thread(target=self.server.serve_forever)
        worker.start()
        self.addCleanup(worker.join)
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)

    def submit(self, text):
        self.sent.append(text)
        return serve.QUEUED

    def post(self, body, *, token="test-token", path="/"):
        connection = http.client.HTTPConnection(*self.server.server_address, timeout=2)
        try:
            headers = {
                "Authorization": "Bearer " + token,
                "Content-Type": "application/json",
            }
            connection.request("POST", path, body, headers)
            response = connection.getresponse()
            return response.status, response.read().decode()
        finally:
            connection.close()

    def test_hex_start_is_a_noop_then_transcript_is_sent_as_one_prompt(self):
        self.assertEqual(self.post('{"text":"  "}')[0], 200)
        self.assertEqual(self.sent, [])

        text = "Check the release.\nReport on “ready” versus blocked."
        self.assertEqual(self.post(json.dumps({"text": text})), (200, serve.QUEUED))
        self.assertEqual(
            self.sent, ["Check the release. Report on “ready” versus blocked."]
        )

    def test_bad_requests_never_reach_phux(self):
        self.assertEqual(self.post('{"text":"valid"}', token="wrong")[0], 401)
        self.assertEqual(self.post('{"text":"valid"}', path="/elsewhere")[0], 404)
        invalid = [
            b"not JSON",
            json.dumps({"text": None}).encode(),
            json.dumps({"text": "press \x1b then send"}).encode(),
            json.dumps({"text": "語" * 1400}).encode(),
        ]
        for body in invalid:
            self.assertEqual(self.post(body)[0], 400)
        self.assertEqual(self.sent, [])


class PromptDelivery(unittest.TestCase):
    @patch("serve.subprocess.run")
    def test_busy_or_blocked_pane_never_gets_input(self, run):
        for state in ("working", "blocked"):
            with self.subTest(state=state):
                run.reset_mock()
                status = json.dumps({"agents": [{"state": state}]}).encode()
                run.return_value = subprocess.CompletedProcess([], 0, status, b"")
                self.assertEqual(
                    serve.submit(["/phux"], "a" * 26, "Please work"), serve.BUSY
                )
                run.assert_called_once_with(
                    ["/phux", "agent", "show", "--json", "^" + "a" * 26],
                    check=True,
                    capture_output=True,
                    timeout=5,
                )

    @patch("serve.readiness", return_value=None)
    @patch("serve.subprocess.run")
    def test_one_guarded_paste_and_no_retry_on_unknown(self, run, _readiness):
        phux = ["/usr/local/bin/phux", "--socket", "/tmp/example.sock"]
        pane_key = "a" * 26
        text = "/test this as a single argument"
        cases = [
            (
                subprocess.CompletedProcess([], 0, b'{"delivery":"acked"}', b""),
                serve.QUEUED,
            ),
            (
                subprocess.CompletedProcess(
                    [], 2, b"", b'{"error":{"code":"no_such_target"}}'
                ),
                serve.UNAVAILABLE,
            ),
            (
                subprocess.CompletedProcess(
                    [],
                    2,
                    b"",
                    b'{"error":{"code":"agent_mismatch",'
                    b'"recovery":{"effect_certainty":"not_applied"}}}',
                ),
                serve.REFUSED,
            ),
            (
                subprocess.CompletedProcess(
                    [], 1, b"", b'{"error":{"code":"delivery_unknown"}}'
                ),
                serve.UNKNOWN,
            ),
            (subprocess.TimeoutExpired("phux", 10), serve.UNKNOWN),
        ]
        for result, message in cases:
            with self.subTest(message=message):
                run.reset_mock()
                run.return_value = result
                run.side_effect = result if isinstance(result, Exception) else None
                self.assertEqual(serve.submit(phux, pane_key, text), message)
                run.assert_called_once_with(
                    phux
                    + [
                        "agent",
                        "prompt",
                        "--json",
                        "--expect-kind",
                        "opencode",
                        "--",
                        "^" + pane_key,
                        serve.PROMPT_PREFIX + text,
                    ],
                    check=False,
                    capture_output=True,
                    timeout=10,
                )


if __name__ == "__main__":
    unittest.main()
