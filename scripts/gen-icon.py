#!/usr/bin/env python3
"""Generate Sluice's app/tray icons (a 'signal/visibility' mark: concentric rings + a center dot
on a dark rounded tile) as RGBA PNGs, with no third-party deps.

Produces the size set the Tauri .deb bundler + the runtime tray/window icon need:
  icon.png (256, used by the code via include_bytes!), 32x32, 128x128, 128x128@2x (256),
  256x256, 512x512  ->  crates/sluice-ui/icons/

Usage: python3 scripts/gen-icon.py
"""
import math
import os
import struct
import zlib

# Geometry as fractions of the canvas, so the mark scales cleanly to any size (tuned at 256px).
DARK = (24, 26, 38)        # tile background
ACCENT = (47, 111, 237)    # rings
BRIGHT = (180, 205, 255)   # center dot
F_HALF = 118.0 / 256.0     # half-size of the rounded tile
F_RAD = 46.0 / 256.0       # corner radius
F_RING_OUT = (78.0 / 256.0, 86.0 / 256.0)
F_RING_IN = (50.0 / 256.0, 58.0 / 256.0)
F_DOT = 20.0 / 256.0       # center dot radius (feathered)


def render(s):
    cx = cy = (s - 1) / 2.0
    b = F_HALF * s
    r = F_RAD * s

    def rrect_sdf(px, py):
        qx = abs(px - cx) - (b - r)
        qy = abs(py - cy) - (b - r)
        return math.hypot(max(qx, 0.0), max(qy, 0.0)) + min(max(qx, qy), 0.0) - r

    def band(d, lo, hi):
        return max(0.0, min(1.0, (d - (lo - 1)))) * max(0.0, min(1.0, ((hi + 1) - d)))

    def mix(bg, fg, a):
        return tuple(round(bg[i] * (1 - a) + fg[i] * a) for i in range(3))

    raw = bytearray()
    for y in range(s):
        raw.append(0)  # PNG filter type 0
        for x in range(s):
            if rrect_sdf(x + 0.5, y + 0.5) > 0.5:
                raw.extend((0, 0, 0, 0))  # transparent outside the tile
                continue
            col = DARK
            d = math.hypot(x - cx, y - cy)
            col = mix(col, ACCENT, 0.95 * band(d, F_RING_OUT[0] * s, F_RING_OUT[1] * s))
            col = mix(col, ACCENT, 0.95 * band(d, F_RING_IN[0] * s, F_RING_IN[1] * s))
            col = mix(col, BRIGHT, max(0.0, min(1.0, F_DOT * s - d)))
            raw.extend((col[0], col[1], col[2], 255))

    def chunk(typ, data):
        body = typ + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", s, s, 8, 6, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(bytes(raw), 9))
    png += chunk(b"IEND", b"")
    return png


def main():
    out_dir = os.path.join(os.path.dirname(__file__), "..", "crates", "sluice-ui", "icons")
    os.makedirs(out_dir, exist_ok=True)
    # (filename, size) — icon.png stays 256 (referenced by the tray/window code via include_bytes!).
    targets = [
        ("icon.png", 256),
        ("32x32.png", 32),
        ("128x128.png", 128),
        ("128x128@2x.png", 256),
        ("256x256.png", 256),
        ("512x512.png", 512),
    ]
    for name, size in targets:
        png = render(size)
        path = os.path.join(out_dir, name)
        with open(path, "wb") as f:
            f.write(png)
        print(f"wrote {os.path.normpath(path)} ({size}x{size}, {len(png)} bytes)")


if __name__ == "__main__":
    main()
