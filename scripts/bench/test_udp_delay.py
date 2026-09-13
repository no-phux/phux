"""Deterministic queue checks and an actual UDP round trip for the path shaper."""

import asyncio
import heapq
import importlib.util
import json
from pathlib import Path
import signal
import socket
import sys
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("udp_delay", Path(__file__).with_name("udp-delay.py"))
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class Timer:
    def __init__(self, callback):
        self.callback = callback
        self.cancelled = False

    def cancel(self):
        self.cancelled = True


class Clock:
    def __init__(self):
        self.now = 0.0
        self.events = []
        self.counter = 0

    def time(self):
        return self.now

    def call_at(self, at, callback):
        timer = Timer(callback)
        self.counter += 1
        heapq.heappush(self.events, (at, self.counter, timer))
        return timer

    def advance(self, end):
        while self.events and self.events[0][0] <= end:
            at, _, timer = heapq.heappop(self.events)
            self.now = at
            if not timer.cancelled:
                timer.callback()
        self.now = end


class ShaperTests(unittest.TestCase):
    def direction(self, **options):
        clock = Clock()
        sent = []
        settings = dict(delay_s=0.025, bytes_per_second=1000, loss_percent=0,
                        seed=17, max_bytes=1000, max_packets=10)
        settings.update(options)
        direction = MODULE.ShapedDirection(
            clock, lambda data, target: sent.append((clock.time(), data, target)), **settings)
        return clock, sent, direction

    def test_serialization_then_delay_preserves_order_and_exact_rate(self):
        clock, sent, direction = self.direction()
        direction.enqueue(b"a" * 100, "peer")
        direction.enqueue(b"b" * 100, "peer")
        clock.advance(0.124)
        self.assertEqual(sent, [])
        clock.advance(0.226)
        self.assertEqual([round(t, 3) for t, _, _ in sent], [0.125, 0.225])
        self.assertEqual([data[:1] for _, data, _ in sent], [b"a", b"b"])
        self.assertEqual(direction.stats["sent_bytes"], 200)
        self.assertEqual(direction.queued_bytes, 0)

    def test_tail_drop_does_not_reserve_serialization_time(self):
        clock, sent, direction = self.direction(max_bytes=100)
        direction.enqueue(b"a" * 100, "peer")
        direction.enqueue(b"b" * 100, "peer")
        clock.advance(0.126)
        direction.enqueue(b"c" * 100, "peer")
        clock.advance(0.252)
        self.assertEqual([data[:1] for _, data, _ in sent], [b"a", b"c"])
        self.assertEqual(direction.stats["queue_drops"], 1)
        self.assertEqual(direction.stats["peak_queue_bytes"], 100)

    def test_packet_limit_bounds_empty_datagrams_and_close_cancels(self):
        clock, sent, direction = self.direction(max_packets=2)
        for _ in range(10):
            direction.enqueue(b"", "peer")
        self.assertEqual(direction.stats["queue_drops"], 8)
        direction.close()
        clock.advance(1)
        self.assertEqual(sent, [])
        self.assertEqual(direction.stats["shutdown_drops"], 2)

    def test_seeded_loss_is_reproducible_and_separate_from_queue_drops(self):
        patterns = []
        for _ in range(2):
            clock, sent, direction = self.direction(loss_percent=30, max_packets=100)
            for index in range(100):
                direction.enqueue(bytes([index]), "peer")
            clock.advance(1)
            patterns.append(sent)
            self.assertGreater(direction.stats["random_drops"], 0)
            self.assertEqual(direction.stats["queue_drops"], 0)
            self.assertEqual(len(sent) + direction.stats["random_drops"], 100)
        self.assertEqual(*patterns)

    def test_directions_have_independent_bandwidth_and_loss(self):
        clock = Clock()
        relay = MODULE.DelayRelay(("127.0.0.1", 1234), 0.025, clock, mbit=0.008)
        relay.directions[0].enqueue(b"x" * 1000, "upstream")
        relay.directions[1].enqueue(b"x", "downstream")
        clock.advance(0.027)
        self.assertEqual(relay.directions[1].stats["sent"], 1)
        self.assertEqual(relay.directions[0].stats["sent"], 0)
        relay.close()


class UdpTests(unittest.IsolatedAsyncioTestCase):
    async def test_delay_only_overflow_invalidates_run(self):
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as reservation:
            reservation.bind(("127.0.0.1", 0))
            port = reservation.getsockname()[1]
        with tempfile.TemporaryDirectory() as directory:
            ready = Path(directory) / "ready"
            process = await asyncio.create_subprocess_exec(
                sys.executable, str(Path(__file__).with_name("udp-delay.py")),
                "--listen", f"127.0.0.1:{port}", "--to", "localhost:9", "--delay-ms", "30000",
                "--max-bytes", "1", "--ready-file", str(ready), stderr=asyncio.subprocess.PIPE)
            try:
                async with asyncio.timeout(3):
                    while not ready.exists():
                        await asyncio.sleep(0.01)
                with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sender:
                    sender.sendto(b"overflow", ("127.0.0.1", port))
                _, stderr = await asyncio.wait_for(process.communicate(), 3)
                self.assertEqual(process.returncode, 2)
                self.assertIn(b"INVALID delay-only experiment", stderr)
            finally:
                if process.returncode is None:
                    process.kill()
                    await process.wait()

    async def test_cli_sigterm_flushes_metrics_and_shutdown_queue(self):
        await self.check_cli_shutdown(signal.SIGTERM)

    async def test_cli_sigint_flushes_metrics_and_shutdown_queue(self):
        await self.check_cli_shutdown(signal.SIGINT)

    async def check_cli_shutdown(self, stop_signal):
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as reservation:
            reservation.bind(("127.0.0.1", 0))
            port = reservation.getsockname()[1]
        with tempfile.TemporaryDirectory() as directory:
            ready = Path(directory) / "ready"
            metrics = Path(directory) / "metrics.json"
            process = await asyncio.create_subprocess_exec(
                sys.executable, str(Path(__file__).with_name("udp-delay.py")),
                "--listen", f"127.0.0.1:{port}", "--to", "localhost:9", "--delay-ms", "30000",
                "--ready-file", str(ready), "--metrics-file", str(metrics))
            try:
                async with asyncio.timeout(3):
                    while not ready.exists():
                        await asyncio.sleep(0.01)
                with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sender:
                    sender.sendto(b"queued", ("127.0.0.1", port))
                await asyncio.sleep(0.05)
                process.send_signal(stop_signal)
                self.assertEqual(await asyncio.wait_for(process.wait(), 3), 0)
                stats = json.loads(metrics.read_text())
                self.assertEqual(stats["upstream"]["received"], 1)
                self.assertEqual(stats["upstream"]["shutdown_drops"], 1)
                self.assertEqual(stats["upstream"]["sent"], 0)
                self.assertEqual(stats["downstream"]["shutdown_drops"], 0)
            finally:
                if process.returncode is None:
                    process.kill()
                    await process.wait()

    async def test_real_bidirectional_echo(self):
        class Echo(asyncio.DatagramProtocol):
            def connection_made(self, transport):
                self.transport = transport

            def datagram_received(self, data, addr):
                self.transport.sendto(data, addr)

        loop = asyncio.get_running_loop()
        reply = loop.create_future()

        class Client(asyncio.DatagramProtocol):
            def datagram_received(self, data, _addr):
                if not reply.done():
                    reply.set_result(data)

        echo, _ = await loop.create_datagram_endpoint(Echo, local_addr=("127.0.0.1", 0))
        relay, protocol = await loop.create_datagram_endpoint(
            lambda: MODULE.DelayRelay(echo.get_extra_info("sockname"), 0.01, loop, mbit=0.3),
            local_addr=("127.0.0.1", 0))
        client, _ = await loop.create_datagram_endpoint(Client, local_addr=("127.0.0.1", 0))
        try:
            payload = bytes(range(256))
            client.sendto(payload, relay.get_extra_info("sockname"))
            self.assertEqual(await asyncio.wait_for(reply, 2), payload)
            self.assertEqual(protocol.metrics()["upstream"]["sent"], 1)
            self.assertEqual(protocol.metrics()["downstream"]["sent"], 1)
        finally:
            protocol.close()
            for transport in (client, relay, echo):
                transport.close()


if __name__ == "__main__":
    unittest.main()
