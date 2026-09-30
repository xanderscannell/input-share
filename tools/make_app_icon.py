# Draw the app icon from the idle desk diagram: a monitor with its lit
# (pale green) screen, the stand, and the teal pointer on the screen.
# Every size is rendered from the shapes (4x4 supersampled), so small sizes
# stay crisp, then packed into one .ico (PNG entries) plus a 256 px PNG.
# Stdlib only. Run: python tools/make_app_icon.py gui/icons
import struct
import sys
import zlib

INK = (31, 43, 51)  # monitor outline, --ink
LIT = (211, 235, 228)  # lit screen, --here-lit
STAND = (154, 168, 178)
TEAL = (11, 122, 99)  # pointer, --here
EDGE = (246, 248, 249)  # pointer outline, --surface

# Design space is 256 x 256.
SCREEN = (16, 44, 224, 144)  # x, y, w, h
RADIUS = 20
# Stand: the diagram's trapezoid (top 24, bottom 36, height 18 on a 168-wide
# screen), scaled to this screen.
STAND_TOP, STAND_BOT, STAND_H = 32, 48, 26
# The diagram's pointer path (18 x 26 units), scaled and centred on the screen.
POINTER = [(0, 0), (0, 22), (6, 16), (10.5, 26), (14.5, 24.2), (10, 14.5), (18, 14.5)]
P_SCALE = 3.6
P_EDGE = 7  # white outline width, design px


def in_rounded(px, py, x, y, w, h, r):
    if not (x <= px <= x + w and y <= py <= y + h):
        return False
    cx = min(max(px, x + r), x + w - r)
    cy = min(max(py, y + r), y + h - r)
    return (px - cx) ** 2 + (py - cy) ** 2 <= r * r


def in_poly(px, py, pts):
    inside = False
    j = len(pts) - 1
    for i in range(len(pts)):
        (xi, yi), (xj, yj) = pts[i], pts[j]
        if (yi > py) != (yj > py) and px < (xj - xi) * (py - yi) / (yj - yi) + xi:
            inside = not inside
        j = i
    return inside


def dist_to_poly_edge(px, py, pts):
    best = 1e9
    for i in range(len(pts)):
        (ax, ay), (bx, by) = pts[i], pts[(i + 1) % len(pts)]
        dx, dy = bx - ax, by - ay
        t = max(0.0, min(1.0, ((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)))
        best = min(best, ((px - ax - t * dx) ** 2 + (py - ay - t * dy) ** 2) ** 0.5)
    return best


sx, sy, sw, sh = SCREEN
pw, ph = 18 * P_SCALE, 26 * P_SCALE
ox, oy = sx + sw / 2 - pw * 0.35, sy + sh / 2 - ph * 0.5
pointer = [(ox + x * P_SCALE, oy + y * P_SCALE) for x, y in POINTER]
stand_y = sy + sh
stand = [
    (128 - STAND_TOP / 2, stand_y),
    (128 + STAND_TOP / 2, stand_y),
    (128 + STAND_BOT / 2, stand_y + STAND_H),
    (128 - STAND_BOT / 2, stand_y + STAND_H),
]


def color_at(px, py, stroke):
    """Topmost shape at a design-space point, or None."""
    if in_poly(px, py, pointer):
        return TEAL
    if dist_to_poly_edge(px, py, pointer) <= P_EDGE / 2 + 1.5:
        return EDGE
    if in_rounded(px, py, sx, sy, sw, sh, RADIUS):
        inner = in_rounded(px, py, sx + stroke, sy + stroke, sw - 2 * stroke, sh - 2 * stroke, max(1, RADIUS - stroke))
        return LIT if inner else INK
    if in_poly(px, py, stand):
        return STAND
    return None


def render(size):
    k = 256 / size
    stroke = max(12, 1.4 * k)  # never thinner than about 1.4 real pixels
    ss = 4
    rows = []
    for j in range(size):
        row = bytearray()
        for i in range(size):
            acc = [0, 0, 0, 0]
            for sj in range(ss):
                for si in range(ss):
                    c = color_at((i + (si + 0.5) / ss) * k, (j + (sj + 0.5) / ss) * k, stroke)
                    if c:
                        acc[0] += c[0]
                        acc[1] += c[1]
                        acc[2] += c[2]
                        acc[3] += 1
            n = acc[3]
            row += bytes([acc[0] // n, acc[1] // n, acc[2] // n, 255 * n // (ss * ss)]) if n else bytes(4)
        rows.append(bytes(row))
    return rows


def png(rows):
    size = len(rows)
    raw = b"".join(b"\x00" + r for r in rows)

    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)

    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")


out = sys.argv[1]
sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256]
images = [png(render(s)) for s in sizes]

# ICO: header, one 16-byte entry per size, then the PNG data. 256 is stored as 0.
offset = 6 + 16 * len(sizes)
ico = struct.pack("<HHH", 0, 1, len(sizes))
for s, data in zip(sizes, images):
    ico += struct.pack("<BBBBHHII", s % 256, s % 256, 0, 0, 1, 32, len(data), offset)
    offset += len(data)
ico += b"".join(images)
open(f"{out}/icon.ico", "wb").write(ico)
open(f"{out}/icon.png", "wb").write(images[-1])
print(f"wrote {out}/icon.ico ({len(sizes)} sizes) and {out}/icon.png (256)")
