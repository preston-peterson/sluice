#!/usr/bin/env python3
"""E0 latency gate — measure the added cost of the cgroup/connect eBPF hook.

DEC-010 was triggered by *seconds* of per-connection latency in a userspace ask-the-UI model. This
isolates the new design's overhead: it times the bare connect(2) syscall to a local listener
in a tight loop. The cgroup/connect4 hook fires inside connect(), so the timed region is
exactly where the BPF program (allow-all + ringbuf push) runs.

Run it TWICE and compare:
  1. BASELINE — with the loader NOT running (no BPF attached)
  2. ATTACHED — with `engine/run-e0.sh` running in another terminal

The delta between the two p50/p99 values is the hook's added latency. Gate: sub-millisecond
(realistically low microseconds) — i.e. nothing remotely like the seconds a userspace per-connection ask adds.

Usage: python3 engine/bench-connect.py [N]   (default N=5000)
"""
import socket
import sys
import time

N = int(sys.argv[1]) if len(sys.argv) > 1 else 5000

srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind(("127.0.0.1", 0))
srv.listen(1024)
addr = srv.getsockname()
srv.setblocking(False)

samples = []
for _ in range(N):
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    t0 = time.perf_counter_ns()
    s.connect(addr)
    t1 = time.perf_counter_ns()
    samples.append(t1 - t0)
    s.close()
    # Drain the accept backlog so listen() never overflows (outside the timed region).
    try:
        while True:
            c, _ = srv.accept()
            c.close()
    except BlockingIOError:
        pass

samples.sort()
us = lambda ns: ns / 1000.0
def pct(p):
    return us(samples[min(int(N * p), N - 1)])

print(f"N={N}  connect(2) latency (microseconds):")
print(f"  p50={pct(0.50):8.2f}  p90={pct(0.90):8.2f}  p99={pct(0.99):8.2f}  "
      f"max={us(samples[-1]):8.2f}  mean={us(sum(samples)//N):8.2f}")
