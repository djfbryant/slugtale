#!/usr/bin/env python3
"""Generate the Slugtale tray icon: a slug silhouette as a macOS template image.

The tray is built with `icon_as_template(true)`, so only the alpha channel
matters — macOS recolors the shape (black in light menu bars, white in dark
ones). We therefore render solid-black pixels and carry the slug shape entirely
in the alpha mask, supersampled for clean anti-aliased edges.

No third-party deps: the PNG is encoded by hand with zlib + crc32.
"""

import math
import struct
import zlib

SIZE = 48          # output icon is SIZE x SIZE px (crisp on retina menu bars)
SS = 4             # supersampling factor per axel (4x4 samples per pixel)
SPACE = 32.0       # design coordinate space; shapes below are authored in 0..32
SCALE = SIZE / SPACE


def circles_along(path, r0, r1, steps=48):
    """Return (cx, cy, r) circles sampled along a poly-path, radius r0 -> r1."""
    # Cumulative length so radius tapers evenly along the whole path.
    seg_len = [math.dist(path[i], path[i + 1]) for i in range(len(path) - 1)]
    total = sum(seg_len) or 1.0
    out = []
    for s in range(steps + 1):
        t = s / steps
        target = t * total
        # Walk segments to find the point at arc-length `target`.
        acc = 0.0
        px, py = path[-1]
        for i, L in enumerate(seg_len):
            if acc + L >= target or i == len(seg_len) - 1:
                u = (target - acc) / (L or 1.0)
                (ax, ay), (bx, by) = path[i], path[i + 1]
                px, py = ax + (bx - ax) * u, ay + (by - ay) * u
                break
            acc += L
        out.append((px, py, r0 + (r1 - r0) * t))
    return out


# --- Slug body: a humped capsule from thin tail (left) to raised head (right).
BODY = [
    (4.2, 21.2, 2.0),    # tail tip
    (7.0, 20.0, 3.3),
    (10.5, 18.6, 4.3),   # hump
    (14.5, 18.1, 4.6),
    (18.5, 18.5, 4.3),
    (21.5, 19.4, 3.7),   # neck
    (24.3, 18.2, 3.3),   # head
]
# Gliding foot (sole): a flat shelf the body rests on.
FOOT = circles_along([(4.5, 22.4), (24.5, 22.4)], 1.7, 1.7, steps=40)

# --- Two eye stalks rising up-and-forward from the head, each with an eye bulb.
STALK_FRONT = circles_along([(24.4, 17.2), (25.6, 12.0), (26.6, 8.8)], 1.25, 0.8)
STALK_BACK = circles_along([(22.3, 17.4), (22.7, 12.6), (22.4, 10.2)], 1.2, 0.78)
EYES = [(26.8, 8.0, 1.7), (22.3, 9.4, 1.55)]

SOLID = BODY + FOOT + STALK_FRONT + STALK_BACK + EYES
FOOT_CLIP = 23.4  # nothing extends below the flat sole


def covered(x, y):
    """True if design-space point (x, y) is inside the slug silhouette."""
    if y > FOOT_CLIP:
        return False
    for cx, cy, r in SOLID:
        dx, dy = x - cx, y - cy
        if dx * dx + dy * dy <= r * r:
            return True
    return False


def alpha_at(px, py):
    """Supersampled coverage (0..255) for output pixel (px, py)."""
    hits = 0
    for sy in range(SS):
        for sx in range(SS):
            dx = (sx + 0.5) / SS
            dy = (sy + 0.5) / SS
            if covered((px + dx) / SCALE, (py + dy) / SCALE):
                hits += 1
    return round(255 * hits / (SS * SS))


def build_png():
    raw = bytearray()
    for py in range(SIZE):
        raw.append(0)  # PNG filter type 0 (none) per scanline
        for px in range(SIZE):
            raw += bytes((0, 0, 0, alpha_at(px, py)))  # black; shape in alpha

    def chunk(tag, data):
        return (struct.pack(">I", len(data)) + tag + data
                + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF))

    ihdr = struct.pack(">IIBBBBB", SIZE, SIZE, 8, 6, 0, 0, 0)  # 8-bit RGBA
    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", ihdr)
            + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
            + chunk(b"IEND", b""))


if __name__ == "__main__":
    import os
    out = os.path.join(os.path.dirname(__file__), "icon.png")
    with open(out, "wb") as f:
        f.write(build_png())

    # Console preview so the shape can be eyeballed without opening the file.
    for py in range(0, SIZE, 2):
        line = "".join(
            " .:-=+*#@"[min(8, alpha_at(px, py) * 9 // 256)]
            for px in range(0, SIZE, 1)
        )
        print(line)
    print(f"\nWrote {out} ({SIZE}x{SIZE})")
