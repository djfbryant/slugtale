#!/usr/bin/env python3
"""Generate the Slugtale icon set from the slug silhouette.

Two families come out of the same geometry:

- Tray template icon (icon.png, 48px): solid-black pixels, shape carried in
  the alpha mask. The tray builds it with icon_as_template(true), so macOS
  recolors it per menu-bar appearance; only alpha matters.
- App-icon bundle (referenced by bundle.icon in tauri.conf.json): the same
  silhouette filled with the app's slug-green, at every size Tauri needs —
  32x32.png, 128x128.png, 128x128@2x.png, icon.icns (macOS) and icon.ico
  (Windows).

No third-party deps: PNG/ICO/ICNS are encoded by hand with zlib + crc32.
Generation takes tens of seconds because large sizes are supersampled in
pure Python; it only needs re-running when the silhouette changes.
"""

import math
import struct
import zlib

SIZE = 48          # tray template icon is SIZE x SIZE px (crisp on retina menu bars)
SS_SMALL = 4       # supersampling factor for app icons below 256px
SS_LARGE = 2       # supersampling factor for app icons 256px and above
SPACE = 32.0       # design coordinate space; shapes below are authored in 0..32


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
FIRST_EYE = len(SOLID) - len(EYES)  # index of the first eye circle in SOLID
FOOT_CLIP = 23.4  # nothing extends below the flat sole
TOP_Y = min(cy - r for _, cy, r in SOLID)

BOUNDS = [(cx - r, cy - r, cx + r, cy + r) for cx, cy, r in SOLID]

# App-icon fill: the settings accent green (#2f9e5f) as a soft vertical
# gradient for depth; the eye bulbs drop darker so they read as eyes.
GRAD_TOP = (0x41, 0xb4, 0x74)
GRAD_BOTTOM = (0x27, 0x86, 0x4c)
EYE_FILL = (0x17, 0x4f, 0x30)


def inside_circle(x, y):
    """Index of the first silhouette circle containing (x, y), or -1."""
    if y > FOOT_CLIP:
        return -1
    for i, ((cx, cy, r), (x0, y0, x1, y1)) in enumerate(zip(SOLID, BOUNDS)):
        if x < x0 or x > x1 or y < y0 or y > y1:
            continue
        dx, dy = x - cx, y - cy
        if dx * dx + dy * dy <= r * r:
            return i
    return -1


def app_fill(x, y, circle_idx):
    if circle_idx >= FIRST_EYE:
        return EYE_FILL
    t = (y - TOP_Y) / (FOOT_CLIP - TOP_Y)
    lo, hi = GRAD_BOTTOM, GRAD_TOP
    return tuple(round(lo[c] + (hi[c] - lo[c]) * t) for c in range(3))


def template_fill(_x, _y, _idx):
    return (0, 0, 0)


def render_raw(size, ss, fill):
    """RGBA scanlines (PNG filter-0 framed) for a size x size slug render."""
    scale = size / SPACE
    denom = ss * ss
    raw = bytearray()
    for py in range(size):
        raw.append(0)  # PNG filter type 0 (none) per scanline
        for px in range(size):
            r_sum = g_sum = b_sum = hits = 0
            for sy in range(ss):
                for sx in range(ss):
                    dx = (sx + 0.5) / ss
                    dy = (sy + 0.5) / ss
                    idx = inside_circle(
                        (px + dx) / scale, (py + dy) / scale)
                    if idx < 0:
                        continue
                    fr, fg, fb = fill(
                        (px + dx) / scale, (py + dy) / scale, idx)
                    r_sum += fr
                    g_sum += fg
                    b_sum += fb
                    hits += 1
            if hits:
                raw += bytes((
                    r_sum // hits, g_sum // hits, b_sum // hits,
                    round(255 * hits / denom)))
            else:
                raw += b"\0\0\0\0"
    return raw


def png_bytes(size, raw):
    def chunk(tag, data):
        return (struct.pack(">I", len(data)) + tag + data
                + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF))

    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)  # 8-bit RGBA
    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", ihdr)
            + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
            + chunk(b"IEND", b""))


_app_cache = {}


def app_icon_png(size):
    """Filled slug app icon at any requested size (cached per size)."""
    if size not in _app_cache:
        ss = SS_SMALL if size < 256 else SS_LARGE
        _app_cache[size] = png_bytes(size, render_raw(size, ss, app_fill))
    return _app_cache[size]


def build_icns(sizes):
    """Apple .icns wrapping PNG-encoded entries (supported macOS 10.9+)."""
    types = {16: b"ic04", 32: b"ic05", 128: b"ic07",
             256: b"ic08", 512: b"ic09", 1024: b"ic10"}
    body = b""
    for size in sizes:
        data = app_icon_png(size)
        body += types[size] + struct.pack(">I", len(data) + 8) + data
    return b"icns" + struct.pack(">I", len(body) + 8) + body


def build_ico(sizes):
    """Windows .ico embedding PNG-compressed images (Vista+ reads these)."""
    count = len(sizes)
    header = struct.pack("<HHH", 0, 1, count)
    directory = b""
    blobs = b""
    offset = 6 + 16 * count
    for size in sizes:
        data = app_icon_png(size)
        dim = 0 if size >= 256 else size  # 0 means 256 in the ICO directory
        directory += struct.pack("<BBBBHHII",
                                 dim, dim, 0, 0, 1, 32, len(data), offset)
        blobs += data
        offset += len(data)
    return header + directory + blobs


if __name__ == "__main__":
    import os
    here = os.path.dirname(__file__)

    # Tray template icon (unchanged shape-in-alpha behaviour).
    with open(os.path.join(here, "icon.png"), "wb") as f:
        f.write(png_bytes(SIZE, render_raw(SIZE, SS_SMALL, template_fill)))

    # App-icon bundle referenced by tauri.conf.json bundle.icon.
    for name, size in [("32x32.png", 32),
                       ("128x128.png", 128),
                       ("128x128@2x.png", 256)]:
        with open(os.path.join(here, name), "wb") as f:
            f.write(app_icon_png(size))
    with open(os.path.join(here, "icon.icns"), "wb") as f:
        f.write(build_icns([16, 32, 128, 256, 512, 1024]))
    with open(os.path.join(here, "icon.ico"), "wb") as f:
        f.write(build_ico([16, 32, 48, 64, 128, 256]))

    print(f"Wrote app-icon bundle to {here}: "
          "32x32.png 128x128.png 128x128@2x.png icon.icns icon.ico")

    # Console preview of the tray template so the shape can be eyeballed.
    preview = render_raw(SIZE, SS_SMALL, template_fill)
    for py in range(0, SIZE, 2):
        line = ""
        for px in range(SIZE):
            off = 1 + py * (SIZE * 4 + 1) + px * 4 + 3  # alpha byte
            line += " .:-=+*#@"[min(8, preview[off] * 9 // 256)]
        print(line)
    print(f"\nWrote icon.png ({SIZE}x{SIZE}, template)")
