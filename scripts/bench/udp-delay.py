#!/usr/bin/env python3
"""Bounded userspace UDP relay for reproducible QUIC path experiments.

Loopback QUIC has a sub-millisecond round trip, so a loopback benchmark
measures per-frame CPU overhead and hides everything that costs a round trip:
a serialized handshake, a per-key request/response, a first paint that will
not fit in one congestion window. Shaping the loopback interface would need
`dnctl`/`pfctl` and therefore root, which a benchmark must not require, so
this relay does the same job entirely in userspace and without privileges.

It binds one UDP socket, forwards every datagram to `--to`, and forwards the
replies back to whichever address last spoke to it, holding each datagram for
`--delay-ms` before it goes out. Delay is applied in *both* directions, so a
relay started with `--delay-ms 25` adds 50 ms to a round trip.

By default the relay adds delay; queue overflow aborts the experiment with a
nonzero exit instead of silently turning a delay-only run into a loss test.
Optional --loss-percent and --mbit model
seeded independent datagram loss and separate full-duplex serialization budgets.
The rate counts UDP payload bytes, excluding IP/UDP/link overhead. Datagrams
remain ordered within each direction. Queues are bounded by bytes AND packets;
tail drops are reported separately from configured random loss. This is a
single-client application-path experiment, not a replacement for a network
emulator or a claim about real WAN performance.

Stdlib only, so it runs from the same scrubbed `env -i` the harness uses.

  udp-delay.py --listen 127.0.0.1:19001 --to 127.0.0.1:19000 --delay-ms 25
"""

import argparse
import asyncio
from collections import deque
import json
import math
import random
import signal
import socket
import sys


def parse_addr(text):
    """Split HOST:PORT into the (host, port) tuple asyncio wants."""
    host, _, port = text.rpartition(":")
    if not host or not port.isdigit():
        raise argparse.ArgumentTypeError("expected HOST:PORT, got %r" % text)
    if not 0 <= int(port) <= 65535:
        raise argparse.ArgumentTypeError("port must be between 0 and 65535")
    return (host, int(port))


class ShapedDirection:
    """One ordered, bounded queue and serialization clock per direction."""

    def __init__(self, loop, send, delay_s, bytes_per_second, loss_percent,
                 seed, max_bytes, max_packets, on_overflow=lambda: None):
        self.loop = loop
        self.send = send
        self.delay_s = delay_s
        self.bytes_per_second = bytes_per_second
        self.loss_percent = loss_percent
        self.random = random.Random(seed)
        self.max_bytes = max_bytes
        self.max_packets = max_packets
        self.on_overflow = on_overflow
        self.queue = deque()
        self.queued_bytes = 0
        self.next_departure = 0.0
        self.timer = None
        self.stats = dict(received=0, sent=0, sent_bytes=0, random_drops=0,
                          queue_drops=0, peak_queue_bytes=0, peak_queue_packets=0,
                          max_queue_age_s=0.0)

    def enqueue(self, data, target):
        self.stats["received"] += 1
        if self.random.random() * 100 < self.loss_percent:
            self.stats["random_drops"] += 1
            return
        if (self.queued_bytes + len(data) > self.max_bytes
                or len(self.queue) >= self.max_packets):
            self.stats["queue_drops"] += 1
            self.on_overflow()
            return
        now = self.loop.time()
        serialization = len(data) / self.bytes_per_second if self.bytes_per_second else 0
        self.next_departure = max(now, self.next_departure) + serialization
        self.queue.append((self.next_departure + self.delay_s, now, data, target))
        self.queued_bytes += len(data)
        self.stats["peak_queue_bytes"] = max(self.stats["peak_queue_bytes"], self.queued_bytes)
        self.stats["peak_queue_packets"] = max(self.stats["peak_queue_packets"], len(self.queue))
        self._arm()

    def _arm(self):
        if self.timer is None and self.queue:
            self.timer = self.loop.call_at(self.queue[0][0], self._drain)

    def _drain(self):
        self.timer = None
        # Bound a callback even when a delayed event loop makes the whole queue due.
        for _ in range(64):
            if not self.queue or self.queue[0][0] > self.loop.time():
                break
            _, received, data, target = self.queue.popleft()
            self.queued_bytes -= len(data)
            self.stats["max_queue_age_s"] = max(
                self.stats["max_queue_age_s"], self.loop.time() - received)
            self.send(data, target)
            self.stats["sent"] += 1
            self.stats["sent_bytes"] += len(data)
        self._arm()

    def close(self):
        if self.timer is not None:
            self.timer.cancel()
            self.timer = None
        self.stats["shutdown_drops"] = len(self.queue)
        self.queue.clear()
        self.queued_bytes = 0


class DelayRelay(asyncio.DatagramProtocol):
    """Forward datagrams between one client and one upstream, delayed.

    A single QUIC client per relay is all the harness needs, so the peer map
    is one slot: whichever address last sent us something that was not the
    upstream is the client. That keeps the relay stateless enough to survive
    the client's connection ID changing mid-flight.
    """

    def __init__(self, upstream, delay_s, loop, *, mbit=0, loss_percent=0,
                 seed=0, max_bytes=4 * 1024 * 1024, max_packets=4096,
                 on_overflow=lambda: None):
        self.upstream = upstream
        self.delay_s = delay_s
        self.loop = loop
        self.transport = None
        self.client = None
        self.forwarded = 0
        self.directions = [
            ShapedDirection(loop, self._send, delay_s, mbit * 1_000_000 / 8,
                            loss_percent, seed + index, max_bytes, max_packets, on_overflow)
            for index in range(2)
        ]

    def connection_made(self, transport):
        self.transport = transport

    def datagram_received(self, data, addr):
        # Compare on the resolved tuple: the client and the upstream can only
        # be told apart by address, and a reply from upstream must go back to
        # the client rather than being echoed at upstream again.
        if addr == self.upstream:
            target = self.client
            direction = self.directions[1]
        else:
            self.client = addr
            target = self.upstream
            direction = self.directions[0]
        if target is None:
            return
        self.forwarded += 1
        direction.enqueue(data, target)

    def _send(self, data, target):
        # The transport is closed only at shutdown, and a datagram scheduled
        # just before that would otherwise raise into the event loop.
        if self.transport is not None and not self.transport.is_closing():
            self.transport.sendto(data, target)

    def error_received(self, exc):
        # A UDP ICMP error (upstream not listening yet) is not fatal: the QUIC
        # client will retransmit its initial and the server will be up by then.
        print("udp-delay: %s" % exc, file=sys.stderr)

    def close(self):
        for direction in self.directions:
            direction.close()

    def metrics(self):
        return dict(zip(("upstream", "downstream"),
                        (direction.stats for direction in self.directions)))


async def run(listen, upstream, delay_s, ready_fd, metrics_file=None, **shaping):
    loop = asyncio.get_running_loop()
    stopped = asyncio.Event()
    delay_only = not shaping.get("mbit", 0) and not shaping.get("loss_percent", 0)
    overflow = stopped.set if delay_only else lambda: None
    addresses = await loop.getaddrinfo(*upstream, family=socket.AF_INET, type=socket.SOCK_DGRAM)
    upstream = addresses[0][4]
    transport, protocol = await loop.create_datagram_endpoint(
        lambda: DelayRelay(upstream, delay_s, loop, on_overflow=overflow, **shaping),
        local_addr=listen,
        family=socket.AF_INET,
    )
    loop.add_signal_handler(signal.SIGTERM, stopped.set)
    try:
        if ready_fd is not None:
            # Signal readiness before the harness starts the client.
            with open(ready_fd, "w", encoding="utf-8") as handle:
                handle.write("ready\n")
        await stopped.wait()
    finally:
        loop.remove_signal_handler(signal.SIGTERM)
        protocol.close()
        transport.close()
        if metrics_file:
            with open(metrics_file, "w", encoding="utf-8") as handle:
                json.dump(protocol.metrics(), handle, indent=2)
    if delay_only and any(direction.stats["queue_drops"] for direction in protocol.directions):
        print("udp-delay: INVALID delay-only experiment: queue overflow; increase queue limits",
              file=sys.stderr)
        return 2
    return 0


def nonnegative(text):
    value = float(text)
    if not math.isfinite(value) or value < 0:
        raise argparse.ArgumentTypeError("must be finite and nonnegative")
    return value


def positive_int(text):
    value = int(text)
    if value < 1:
        raise argparse.ArgumentTypeError("must be positive")
    return value


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--listen", type=parse_addr, required=True)
    ap.add_argument("--to", dest="upstream", type=parse_addr, required=True)
    ap.add_argument("--delay-ms", type=nonnegative, required=True,
                    help="one-way delay; a round trip pays it twice")
    ap.add_argument("--ready-file", help="write 'ready' here once bound")
    ap.add_argument("--mbit", type=nonnegative, default=0,
                    help="UDP payload Mbit/s per direction; 0 means unlimited")
    ap.add_argument("--loss-percent", type=nonnegative, default=0)
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--max-bytes", type=positive_int, default=4 * 1024 * 1024)
    ap.add_argument("--max-packets", type=positive_int, default=4096)
    ap.add_argument("--metrics-file", help="write direction counters at shutdown")
    args = ap.parse_args()
    if args.loss_percent > 100:
        ap.error("--loss-percent must be at most 100")
    try:
        return asyncio.run(run(args.listen, args.upstream, args.delay_ms / 1000.0,
                         args.ready_file, args.metrics_file, mbit=args.mbit,
                         loss_percent=args.loss_percent, seed=args.seed,
                         max_bytes=args.max_bytes, max_packets=args.max_packets))
    except KeyboardInterrupt:
        return 0


if __name__ == "__main__":
    sys.exit(main())
