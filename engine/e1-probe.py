#!/usr/bin/env python3
"""E1 test helper — a loopback TCP listener and a connect probe.

  listen PORT        bind 127.0.0.1:PORT and accept forever (run in the background)
  probe  PORT [HOST] attempt one connect to HOST:PORT (default 127.0.0.1); print the result

A cgroup/connect deny makes connect(2) return EPERM, which Python raises as PermissionError,
so a blocked target prints `DENIED:PermissionError`. A normal connect prints `OK`.
"""
import socket
import sys

mode = sys.argv[1] if len(sys.argv) > 1 else ""
port = int(sys.argv[2]) if len(sys.argv) > 2 else 0
host = sys.argv[3] if len(sys.argv) > 3 else "127.0.0.1"

if mode == "listen":
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    # Bind all interfaces (0.0.0.0 includes loopback) so the E6 netns peer can reach it too.
    s.bind(("0.0.0.0", port))
    s.listen(128)
    while True:
        try:
            c, _ = s.accept()
            c.close()
        except OSError:
            break
elif mode == "probe":
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.settimeout(3)
    try:
        s.connect((host, port))
        s.close()
        print("OK")
    except PermissionError:
        print("DENIED:PermissionError")
    except Exception as e:  # noqa: BLE001 — report whatever else the kernel/network returned
        print(f"OTHER:{type(e).__name__}")
else:
    print("usage: e1-probe.py [listen|probe] PORT [HOST]", file=sys.stderr)
    sys.exit(2)
