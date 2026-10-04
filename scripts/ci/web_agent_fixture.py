"""Test-only local owner for browser AgentSession lifecycle acceptance.

The browser is not a producer: only the disposable server's Unix owner can
append records. This loopback fixture offers canned actions, never arbitrary
commands, behind an unguessable per-run path. It owns no production state.
"""

from http.server import BaseHTTPRequestHandler, HTTPServer
import secrets
import subprocess
import threading


class AgentFixture:
    def __init__(self, binary, socket_path, env):
        self.binary = str(binary)
        self.socket_path = str(socket_path)
        self.env = env
        self.token = secrets.token_urlsafe(24)
        self.opened = False
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                prefix = f"/{owner.token}/"
                if not self.path.startswith(prefix):
                    self.send_error(404)
                    return
                try:
                    owner.action(self.path[len(prefix):])
                    code, body = 200, b"ok"
                except (ValueError, subprocess.SubprocessError) as error:
                    code, body = 500, str(error).encode()
                self.send_response(code)
                self.send_header("Access-Control-Allow-Origin", "*")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, _format, *_args):
                pass

        self.server = HTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.url = f"http://127.0.0.1:{self.server.server_port}/{self.token}"

    def command(self, *args):
        result = subprocess.run(
            [self.binary, *args, "--socket", self.socket_path],
            env=self.env, capture_output=True, text=True, timeout=10,
        )
        if result.returncode:
            raise ValueError(result.stderr)

    def action(self, action):
        if action == "open" and not self.opened:
            self.command("agent", "session", "open", "default", "--provider", "pi")
            self.opened = True
            self.command("agent", "emit", "default", "--type", "session_start",
                         "--data", '{"provider":"pi"}')
        elif action == "close" and self.opened:
            self.command("agent", "session", "close", "default")
            self.opened = False
        elif action in ("prompt", "ask", "stop") and self.opened:
            self.command("agent", "emit", "default", "--type", action)
        else:
            raise ValueError("invalid fixture action or lifecycle")

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, _kind, _value, _traceback):
        self.server.shutdown()
        self.thread.join(timeout=12)
        self.server.server_close()
        if self.opened:
            self.action("close")
