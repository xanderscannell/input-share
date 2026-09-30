# Draw the tray icons (32x32 RGBA PNG) as a tiny version of the app's desk
# diagram: two screens side by side, the one with the pointer lit.
#   idle: both outlines.
#   here-left / here-right: this computer's screen teal, on that side.
#   away-left / away-right: the other computer's screen amber, on that side.
# The window picks the variant that matches its own diagram, which puts this
# computer on whichever side Settings says.
# Stdlib only. Run: python tools/make_tray_icons.py gui/icons
import struct
import sys
import zlib

SIZE, SS = 32, 4  # 4x4 supersampling for antialiasing
OUTLINE = (154, 168, 178)  # mid slate, readable on light and dark taskbars
TEAL = (31, 174, 140)
AMBER = (240, 164, 69)

# Screens: (left, top, width, height), rounded corners, 2 px outline.
SCREENS = [(1, 7, 14, 11), (17, 7, 14, 11)]
RADIUS, STROKE = 2.5, 2.0


def in_rounded(px, py, x, y, w, h, r):
    if not (x <= px <= x + w and y <= py <= y + h):
        return False
    cx = min(max(px, x + r), x + w - r)
    cy = min(max(py, y + r), y + h - r)
    return (px - cx) ** 2 + (py - cy) ** 2 <= r * r


def color_at(px, py, fills):
    for i, (x, y, w, h) in enumerate(SCREENS):
        if in_rounded(px, py, x, y, w, h, RADIUS):
            inner = in_rounded(px, py, x + STROKE, y + STROKE, w - 2 * STROKE, h - 2 * STROKE, RADIUS - 1)
            if not inner:
                return OUTLINE
            return fills[i]  # None: transparent inside
        # Stand: a short post and foot under each screen.
        mid = x + w / 2
        if abs(px - mid) <= 1 and y + h < py <= y + h + 3:
            return OUTLINE
        if abs(px - mid) <= 3.5 and y + h + 3 < py <= y + h + 5:
            return OUTLINE
    return None


def render(fills):
    rows = []
    for j in range(SIZE):
        row = bytearray()
        for i in range(SIZE):
            acc = [0, 0, 0, 0]
            for sj in range(SS):
                for si in range(SS):
                    c = color_at(i + (si + 0.5) / SS, j + (sj + 0.5) / SS, fills)
                    if c:
                        acc[0] += c[0]
                        acc[1] += c[1]
                        acc[2] += c[2]
                        acc[3] += 1
            n = acc[3]
            if n:
                row += bytes([acc[0] // n, acc[1] // n, acc[2] // n, 255 * n // (SS * SS)])
            else:
                row += bytes(4)
        rows.append(bytes(row))
    return rows


def png(rows):
    raw = b"".join(b"\x00" + r for r in rows)

    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)

    ihdr = struct.pack(">IIBBBBB", SIZE, SIZE, 8, 6, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")


out = sys.argv[1]
for name, fills in [
    ("idle", [None, None]),
    ("here-left", [TEAL, None]),
    ("here-right", [None, TEAL]),
    ("away-left", [AMBER, None]),
    ("away-right", [None, AMBER]),
]:
    with open(f"{out}/tray-{name}.png", "wb") as f:
        f.write(png(render(fills)))
    print(f"wrote {out}/tray-{name}.png")
