"""Generates the Juttusihti icon (a crosshair over a news page) as PNGs.

Pure Python (zlib + struct), no image libraries. Run:
    python packaging/make_icon.py packaging/icons
"""
import math
import os
import struct
import sys
import zlib

BG = (24, 52, 92)        # deep blue
PAGE = (236, 240, 245)   # paper
LINE = (150, 165, 185)   # text lines
RING = (230, 92, 60)     # sight, warm red


def png(path, size, pixels):
    raw = b"".join(b"\x00" + bytes(c for px in row for c in px) for row in pixels)

    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)

    data = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
    data += chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")
    with open(path, "wb") as f:
        f.write(data)


def blend(dst, src, a):
    return tuple(round(d * (1 - a) + s * a) for d, s in zip(dst, src))


def render(size):
    s = size
    ss = 4  # supersampling per axis
    rows = []
    r_corner = 0.18 * s
    cx = cy = s / 2
    for y in range(s):
        row = []
        for x in range(s):
            acc = [0.0, 0.0, 0.0, 0.0]
            for sy in range(ss):
                for sx in range(ss):
                    px, py = x + (sx + 0.5) / ss, y + (sy + 0.5) / ss
                    # Rounded-square background.
                    dx = max(r_corner - px, 0, px - (s - r_corner))
                    dy = max(r_corner - py, 0, py - (s - r_corner))
                    if dx * dx + dy * dy > r_corner * r_corner:
                        continue
                    c = BG
                    # News page.
                    if 0.24 * s <= px <= 0.76 * s and 0.20 * s <= py <= 0.80 * s:
                        c = PAGE
                        for i, ly in enumerate((0.30, 0.40, 0.50, 0.60, 0.70)):
                            width = 0.42 if i % 2 == 0 else 0.34
                            if abs(py - ly * s) < 0.022 * s and 0.29 * s <= px <= (0.29 + width) * s:
                                c = LINE
                    d = math.hypot(px - cx, py - cy)
                    # Sight ring and cross hairs with a gap in the middle.
                    if abs(d - 0.30 * s) < 0.035 * s:
                        c = RING
                    if (abs(px - cx) < 0.018 * s or abs(py - cy) < 0.018 * s) and 0.08 * s < d < 0.42 * s:
                        c = RING
                    if d < 0.03 * s:
                        c = RING
                    acc[0] += c[0]
                    acc[1] += c[1]
                    acc[2] += c[2]
                    acc[3] += 1
            n = ss * ss
            if acc[3] == 0:
                row.append((0, 0, 0, 0))
            else:
                row.append((round(acc[0] / acc[3]), round(acc[1] / acc[3]), round(acc[2] / acc[3]), round(255 * acc[3] / n)))
        rows.append(row)
    return rows


if __name__ == "__main__":
    out = sys.argv[1] if len(sys.argv) > 1 else "icons"
    os.makedirs(out, exist_ok=True)
    for size in (32, 64, 128, 256, 512):
        png(os.path.join(out, f"juttusihti-{size}.png"), size, render(size))
    print("icons written to", out)
