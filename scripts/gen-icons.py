"""Generate the application icons (common/assets/*.ico) from code, so they can
be rebuilt without an image editor: a rounded square with a diagonal gradient
and a white "N", like the logo in the web pages (common/web/src/lib/theme.css).

    python common/scripts/gen-icons.py

client.ico: pink → orange (the launcher's accent); server.ico: blue → teal,
so the two programs are easy to tell apart. Standard library only.
"""

import os
import struct
import zlib

SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]
SUPERSAMPLE = 4

ICONS = {
    "client.ico": ((0xE8, 0x57, 0x7A), (0xF5, 0x9E, 0x6B)),
    "server.ico": ((0x3B, 0x6F, 0xE0), (0x2F, 0xB5, 0xA6)),
}


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


def n_glyph(size):
    """The letter N as three polygons (left stem, diagonal, right stem)."""
    s = size
    left, right = 0.27 * s, 0.73 * s
    top, bottom = 0.24 * s, 0.76 * s
    w = 0.115 * s
    stem_l = [(left, top), (left + w, top), (left + w, bottom), (left, bottom)]
    stem_r = [(right - w, top), (right, top), (right, bottom), (right - w, bottom)]
    diag = [(left, top), (left + w * 1.15, top), (right, bottom), (right - w * 1.15, bottom)]
    return [stem_l, diag, stem_r]


def render(size, c0, c1):
    ss = size * SUPERSAMPLE
    radius = 0.23 * ss
    glyph = n_glyph(ss)
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
                    if any(inside_poly(x, y, p) for p in glyph):
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
