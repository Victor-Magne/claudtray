#!/usr/bin/env python3
"""Regenerate the ClaudTray app icons from the 'aperture' mark.

Run from the repo root:  python3 assets/gen_icons.py

Writes:
  assets/claudtray.ico          (16/32/48/64/128/256, PNG-compressed entries)
  packaging/arch/claudtray.png  (256x256)

The mark is the same broken gauge ring the tray renderer draws
(src/renderer.rs) — a ring with a ~60 deg wedge cut out of the bottom —
here in off-white on the dark #111827 rounded tile. Stdlib only."""

import math
import struct
import zlib

TILE = (17, 24, 39)     # #111827
RING = (232, 234, 237)  # #E8EAED
SS = 4                   # supersampling per axis

R_OUTER = 0.375
R_INNER = 0.25
CORNER = 0.2237         # rounded-square radius / size


def rrect_inside(x, y, s, r):
    dx = abs(x - s / 2) - (s / 2 - r)
    dy = abs(y - s / 2) - (s / 2 - r)
    return math.hypot(max(dx, 0.0), max(dy, 0.0)) + min(max(dx, dy), 0.0) - r <= 0.0


def ring_inside(x, y, s):
    dx = x - s / 2
    dy = y - s / 2
    d = math.hypot(dx, dy)
    if not (R_INNER * s < d < R_OUTER * s):
        return False
    # 60 deg wedge removed at the bottom: dy>0 and |dx| < dy/sqrt(3)
    return not (dy > 0 and dx * dx * 3.0 < dy * dy)


def render(size):
    r = CORNER * size
    out = bytearray()
    n = SS * SS
    for y in range(size):
        out.append(0)  # PNG filter: none
        for x in range(size):
            tile = ring = 0
            for sy in range(SS):
                for sx in range(SS):
                    fx = x + (sx + 0.5) / SS
                    fy = y + (sy + 0.5) / SS
                    if rrect_inside(fx, fy, size, r):
                        tile += 1
                        if ring_inside(fx, fy, size):
                            ring += 1
            ct, cr = tile / n, ring / n
            if ct == 0.0:
                out += b"\x00\x00\x00\x00"
                continue
            out += bytes(
                max(0, min(255, round((RING[i] * cr + TILE[i] * (ct - cr)) / ct)))
                for i in range(3)
            ) + bytes([round(ct * 255)])
    return bytes(out)


def _chunk(tag, data):
    return struct.pack(">I", len(data)) + tag + data + struct.pack(
        ">I", zlib.crc32(tag + data) & 0xFFFFFFFF
    )


def png(size, raw):
    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return (b"\x89PNG\r\n\x1a\n" + _chunk(b"IHDR", ihdr)
            + _chunk(b"IDAT", zlib.compress(raw, 9)) + _chunk(b"IEND", b""))


def build_ico(items):
    header = struct.pack("<HHH", 0, 1, len(items))
    offset = 6 + len(items) * 16
    entries = body = b""
    for size, data in items:
        entries += struct.pack("<BBBBHHII", size & 0xFF, size & 0xFF, 0, 0, 1, 32,
                               len(data), offset)
        offset += len(data)
        body += data
    return header + entries + body


if __name__ == "__main__":
    pngs = {s: png(s, render(s)) for s in (16, 32, 48, 64, 128, 256)}
    with open("assets/claudtray.ico", "wb") as f:
        f.write(build_ico([(s, pngs[s]) for s in (16, 32, 48, 64, 128, 256)]))
    with open("packaging/arch/claudtray.png", "wb") as f:
        f.write(pngs[256])
    print("wrote assets/claudtray.ico + packaging/arch/claudtray.png")
