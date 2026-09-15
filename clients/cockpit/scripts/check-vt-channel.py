#!/usr/bin/env python3
"""Refuse VT / PANE_OUTPUT on the Native SDK 4096 effect channel.

Production providers/phux: the module owns the socket, complete frames
cross reusable queues, only a one-byte wake is posted, and the UI thread
drains and feeds the engine. See docs/DECISIONS.md.
"""

from pathlib import Path
import re
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
PHUX = ROOT / "src" / "providers" / "phux"
HANDLERS = (
    ROOT / "src" / "cockpit" / "update.zig",
    ROOT / "src" / "cockpit" / "native" / "ts_engine.zig",
)
TRANSPORT = PHUX / "transport.zig"

POST_CALL = re.compile(r"\.post\s*\(")
ALLOWED_POST = re.compile(r"\.post\s*\(\s*&(?:transport\.)?wake_payload\s*\)")
WAKE_DEF = re.compile(r"pub const wake_payload\s*=\s*(\[_\]u8\{[^}]*\}(?:\s*\*\*\s*\S+)?)")
ALLOWED_WAKE = {"[_]u8{1}", "[_]u8{2}"}
EVENT_BYTES = re.compile(r"\bevent\.bytes\b")
DRAIN = re.compile(r"\b(?:drainReadiness|drainPhux)\b")


def strip_comments(text):
    text = re.sub(r"/\*.*?\*/", " ", text, flags=re.DOTALL)
    text = re.sub(r"<!--.*?-->", " ", text, flags=re.DOTALL)
    return re.sub(r"//.*?$", " ", text, flags=re.MULTILINE)


def zig_files(directory):
    for path in sorted(directory.rglob("*.zig")):
        if path.is_file():
            yield path


def extract_balanced(text, start):
    brace = text.find("{", start)
    if brace < 0:
        return ""
    depth = 0
    for index, char in enumerate(text[brace:], brace):
        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                return text[brace : index + 1]
    return text[brace:]


def phux_handler_blocks(text):
    blocks = []
    for needle in (".phux_channel =>", "fn onPhuxChannel"):
        start = 0
        while True:
            index = text.find(needle, start)
            if index < 0:
                break
            blocks.append(extract_balanced(text, index))
            start = index + len(needle)
    return blocks


def compact(text):
    return re.sub(r"\s+", "", text)


def scan_posts(directory):
    hits = []
    for path in zig_files(directory):
        code = strip_comments(path.read_text(encoding="utf-8"))
        rel = path.relative_to(directory).as_posix()
        for match in POST_CALL.finditer(code):
            rest = code[match.start() :]
            if ALLOWED_POST.match(rest):
                continue
            line = rest.splitlines()[0].strip()
            hits.append(f"{rel}: VT/PANE_OUTPUT must not ride the 4096 channel: {line}")
        for match in WAKE_DEF.finditer(code):
            if compact(match.group(1)) not in ALLOWED_WAKE:
                hits.append(f"{rel}: wake_payload must be exactly one byte")
    return hits


def scan_handlers(paths):
    hits = []
    for path in paths:
        code = strip_comments(path.read_text(encoding="utf-8"))
        try:
            rel = path.relative_to(ROOT).as_posix()
        except ValueError:
            rel = path.name
        blocks = phux_handler_blocks(code)
        if not blocks:
            hits.append(f"{rel}: missing phux channel drain")
            continue
        for block in blocks:
            if EVENT_BYTES.search(block):
                hits.append(f"{rel}: phux channel handler feeds event.bytes as VT")
            if not DRAIN.search(block):
                hits.append(f"{rel}: phux channel handler must drain the frame queue")
    return hits


class VtChannel(unittest.TestCase):
    def test_product_phux_posts_only_the_one_byte_wake(self):
        self.assertEqual(scan_posts(PHUX), [])

    def test_transport_wake_is_one_byte(self):
        source = TRANSPORT.read_text(encoding="utf-8")
        self.assertRegex(source, r"pub const wake_payload = \[_\]u8\{1\};")

    def test_phux_channel_handlers_drain_queues_not_payloads(self):
        self.assertEqual(scan_handlers(HANDLERS), [])

    def test_pane_output_chunk_post_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "extension.zig"
            path.write_text(
                "fn wake(handle: anytype, frame: []const u8) void {\n"
                "    var offset: usize = 0;\n"
                "    while (offset < frame.len) {\n"
                "        const end = @min(offset + 4096, frame.len);\n"
                "        _ = handle.post(frame[offset..end]);\n"
                "        offset = end;\n"
                "    }\n"
                "}\n"
            )
            self.assertIn("VT/PANE_OUTPUT must not ride the 4096 channel", "\n".join(scan_posts(Path(directory))))

    def test_event_bytes_feed_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "update.zig"
            path.write_text(
                "pub fn onPhuxChannel(event: anytype, session: anytype) void {\n"
                "    session.feed(event.bytes);\n"
                "}\n"
            )
            self.assertIn(
                "phux channel handler feeds event.bytes as VT",
                "\n".join(scan_handlers([path])),
            )

    def test_handler_without_queue_drain_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "engine.zig"
            path.write_text(
                "pub fn onPhuxChannel(event: anytype) void {\n"
                "    _ = event;\n"
                "}\n"
            )
            self.assertIn(
                "must drain the frame queue",
                "\n".join(scan_handlers([path])),
            )

    def test_widened_wake_payload_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "transport.zig"
            path.write_text("pub const wake_payload = [_]u8{1} ** 4096;\n")
            self.assertIn("wake_payload must be exactly one byte", "\n".join(scan_posts(Path(directory))))

    def test_wake_posts_and_refusal_comments_are_allowed(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "extension.zig"
            path.write_text(
                "//! Do not post PANE_OUTPUT through the 4096-byte channel.\n"
                "fn wake(handle: anytype) void {\n"
                "    _ = handle.post(&transport.wake_payload);\n"
                "}\n"
                "pub const wake_payload = [_]u8{1};\n"
            )
            self.assertEqual(scan_posts(Path(directory)), [])


if __name__ == "__main__":
    unittest.main()
