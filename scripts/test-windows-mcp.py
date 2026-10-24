"""Qualify native Windows MCP stdio without PowerShell/.NET text encoders."""

import json
import queue
import subprocess
import sys
import threading


def qualify(distribution: str) -> None:
    assert sys.platform == "win32", "This qualification must run in native Windows Python"
    command = [
        "wsl.exe", "--distribution", distribution, "--exec", "sh", "-c",
        "exec ~/.local/bin/phux-mcp",
    ]
    child = subprocess.Popen(
        command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        creationflags=subprocess.CREATE_NO_WINDOW,
    )
    print(f"Native Windows MCP child pid: {child.pid}", flush=True)
    assert child.stdin is not None and child.stdout is not None and child.stderr is not None
    replies: queue.Queue[bytes] = queue.Queue()
    threading.Thread(target=lambda: replies.put(child.stdout.readline()), daemon=True).start()
    try:
        request = {
            "jsonrpc": "2.0", "id": "\U0001f355", "method": "initialize",
            "params": {"protocolVersion": "2025-03-26", "capabilities": {},
                       "clientInfo": {"name": "windows-native-ci", "version": "1"}},
        }
        child.stdin.write((json.dumps(request, ensure_ascii=False) + "\n").encode("utf-8"))
        child.stdin.flush()
        # Receive a reply BEFORE closing stdin: a buffered relay cannot pass.
        response = replies.get(timeout=20)
        if not response:
            raise AssertionError(f"MCP closed stdout: {child.stderr.read().decode('utf-8', 'replace')}")
        reply = json.loads(response)
        assert reply["id"] == request["id"], reply
        assert reply["result"]["serverInfo"]["name"] == "phux", reply
        child.stdin.close()
        assert child.wait(timeout=10) == 0, child.stderr.read().decode("utf-8", "replace")
        print("Native Windows MCP full-duplex UTF-8 stdio passed.", flush=True)
    finally:
        child.stdin.close()
        try:
            child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            child.kill()  # Only the exact child PID this test created.
            child.wait(timeout=5)


if __name__ == "__main__":
    qualify(sys.argv[1])
