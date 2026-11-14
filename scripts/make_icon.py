"""Draws Monaged's app icon (assets/monaged.ico) without any image library.

A near-black rounded square with the red dot from the title bar and, at
larger sizes, a dot-matrix "M" like the app's big numbers. Each size is
drawn at 4x and averaged down for smooth edges; the .ico holds PNGs.
"""
import struct
import sys
import zlib
from pathlib import Path

BG = (15, 15, 15)
EDGE = (47, 47, 47)
RED = (215, 25, 33)
WHITE = (237, 237, 237)

# 5x7 "M", same grid as the app's dot-matrix numerals.
M = ["10001", "11011", "10101", "10101", "10001", "10001", "10001"]


def draw(size):
    s = size * 4
    px = [[(0, 0, 0, 0)] * s for _ in range(s)]
    r = s * 0.22  # corner radius
    for y in range(s):
        for x in range(s):
            # Rounded square with a hairline edge.
            dx = max(r - x, 0, x - (s - 1 - r))
            dy = max(r - y, 0, y - (s - 1 - r))
            d = (dx * dx + dy * dy) ** 0.5
            if d <= r:
                edge = d > r - s * 0.012 or min(x, y, s - 1 - x, s - 1 - y) < s * 0.012
                px[y][x] = (*(EDGE if edge else BG), 255)
    def disc(cx, cy, radius, color):
        for y in range(int(cy - radius) - 1, int(cy + radius) + 2):
            for x in range(int(cx - radius) - 1, int(cx + radius) + 2):
                if 0 <= x < s and 0 <= y < s and (x - cx) ** 2 + (y - cy) ** 2 <= radius * radius:
                    px[y][x] = (*color, 255)
    if size < 48:
        disc(s / 2, s / 2, s * 0.2, RED)
    else:
        # The M, centred a little low; the red dot top left, as in the title bar.
        cell = s * 0.08
        left = s / 2 - 2 * cell + s * 0.02
        top = s * 0.27
        for row, line in enumerate(M):
            for col, on in enumerate(line):
                if on == "1":
                    disc(left + col * cell, top + row * cell, cell * 0.36, WHITE)
        disc(s * 0.19, s * 0.17, s * 0.065, RED)
    # 4x4 average down.
    out = []
    for y in range(size):
        row = []
        for x in range(size):
            acc = [0, 0, 0, 0]
            for yy in range(4):
                for xx in range(4):
                    p = px[y * 4 + yy][x * 4 + xx]
                    a = p[3]
                    acc[0] += p[0] * a
                    acc[1] += p[1] * a
                    acc[2] += p[2] * a
                    acc[3] += a
            a = acc[3] / 16
            row.append(tuple(int(c / acc[3]) if acc[3] else 0 for c in acc[:3]) + (int(a),))
        out.append(row)
    return out


def png(pixels):
    h, w = len(pixels), len(pixels[0])
    raw = b"".join(b"\x00" + bytes(c for p in row for c in p) for row in pixels)
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")


def ico(images):
    header = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    entries, data = b"", b""
    for size, blob in images:
        entries += struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(blob), offset + len(data))
        data += blob
    return header + entries + data


if __name__ == "__main__":
    out = Path(sys.argv[1] if len(sys.argv) > 1 else "assets/monaged.ico")
    sizes = [16, 24, 32, 48, 64, 128, 256]
    out.write_bytes(ico([(n, png(draw(n))) for n in sizes]))
    (out.parent / "monaged-256.png").write_bytes(png(draw(256)))
    print(f"wrote {out}")
