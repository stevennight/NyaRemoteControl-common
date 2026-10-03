"""Generate the application icons (common/assets/*.ico) from code, so they can
be rebuilt without an image editor: a rounded square with a diagonal gradient
and, in white, the Android app's mark — a monitor with cat ears and a mouse
pointer (android/app/src/main/res/drawable/ic_launcher_foreground.xml, whose
108-unit coordinates are used here); the screen shows the gradient.

    python common/scripts/gen-icons.py

The Windows program and its service are one product: both icons are red
(pink → orange, the launcher's accent); the Android app is blue. Standard
library only.
"""

import os
import struct
import zlib

SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]
SUPERSAMPLE = 4

RED = ((0xE8, 0x57, 0x7A), (0xF5, 0x9E, 0x6B))
ICONS = {
    "client.ico": RED,
    "server.ico": RED,
}

# The Android mark's 108-unit canvas: this window of it fills the icon.
VIEW0, VIEW1 = 20.0, 88.0


def inside_rounded(x, y, size, r):
    cx = min(max(x, r), size - r)
    cy = min(max(y, r), size - r)
    return (x - cx) ** 2 + (y - cy) ** 2 <= r * r


def inside_poly(x, y, pts):
    n = len(pts)
    c = False
    j = n - 1
    for i in range(n):
        xi, yi = pts[i]
        xj, yj = pts[j]
        if (yi > y) != (yj > y) and x < (xj - xi) * (y - yi) / (yj - yi) + xi:
            c = not c
        j = i
    return c


def in_round_rect(x, y, x0, y0, x1, y1, r):
    if not (x0 <= x <= x1 and y0 <= y <= y1):
        return False
    cx = min(max(x, x0 + r), x1 - r)
    cy = min(max(y, y0 + r), y1 - r)
    return (x - cx) ** 2 + (y - cy) ** 2 <= r * r


EARS = [[(36, 38), (41, 27), (47, 38)], [(61, 38), (67, 27), (72, 38)]]
STAND = [(48, 68), (60, 68), (62, 76), (46, 76)]
CURSOR = [(50, 44), (50, 58), (53.5, 54.5), (56, 60), (58.5, 59), (56, 53.5), (61, 53.5)]


def is_mark(u, v):
    """White at (u, v) in the Android canvas's coordinates?"""
    if inside_poly(u, v, CURSOR):
        return True
    if 34 <= u <= 74 and 40 <= v <= 62:
        return False  # the screen: shows the background
    return (
        in_round_rect(u, v, 28, 36, 80, 68, 4)
        or any(inside_poly(u, v, e) for e in EARS)
        or inside_poly(u, v, STAND)
        or (42 <= u <= 66 and 76 <= v <= 79)
    )


def render(size, c0, c1):
    ss = size * SUPERSAMPLE
    radius = 0.23 * ss
    scale = (VIEW1 - VIEW0) / ss
    rows = []
    for py in range(size):
        row = bytearray()
        for px in range(size):
            acc = [0.0, 0.0, 0.0, 0.0]
            for sy in range(SUPERSAMPLE):
                for sx in range(SUPERSAMPLE):
                    x = px * SUPERSAMPLE + sx + 0.5
                    y = py * SUPERSAMPLE + sy + 0.5
                    if not inside_rounded(x, y, ss, radius):
                        continue
                    if is_mark(VIEW0 + x * scale, VIEW0 + y * scale):
                        col = (255, 255, 255)
                    else:
                        t = (x + y) / (2 * ss)
                        col = tuple(c0[i] + (c1[i] - c0[i]) * t for i in range(3))
                    acc[0] += col[0]
                    acc[1] += col[1]
                    acc[2] += col[2]
                    acc[3] += 1
            n = SUPERSAMPLE * SUPERSAMPLE
            if acc[3]:
                row += bytes([round(acc[0] / acc[3]), round(acc[1] / acc[3]), round(acc[2] / acc[3]), round(255 * acc[3] / n)])
            else:
                row += bytes(4)
        rows.append(bytes(row))
    return rows


def png(size, rows):
    raw = b"".join(b"\x00" + r for r in rows)

    def chunk(tag, data):
        return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)

    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")


def ico(images):
    head = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    entries, data = b"", b""
    for size, blob in images:
        dim = 0 if size >= 256 else size
        entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(blob), offset + len(data))
        data += blob
    return head + entries + data


def main():
    out = os.path.join(os.path.dirname(__file__), "..", "assets")
    os.makedirs(out, exist_ok=True)
    for name, (c0, c1) in ICONS.items():
        images = [(s, png(s, render(s, c0, c1))) for s in SIZES]
        path = os.path.join(out, name)
        with open(path, "wb") as f:
            f.write(ico(images))
        print("wrote", os.path.normpath(path))


if __name__ == "__main__":
    main()
