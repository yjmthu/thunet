#!/usr/bin/env python3
"""Generate the application icons used by the Windows and Linux packages.

The tray icon is drawn at runtime (`make_icon` in `src/ui.rs`) as a plain disc,
so the packaging assets reuse the same idea: a green disc with a white "T".

Run from the repository root:

    pixi run -e tools python scripts/make_icon.py

Outputs:
    assets/thunet.ico   16/32/48/64/128/256 px, PNG-compressed entries
    assets/thunet.png   256 px preview (used by some desktop environments)
    android/app/src/main/res/mipmap-*/ic_launcher.png
    android/app/src/main/res/mipmap-*/ic_launcher_foreground.png
    android/app/src/main/res/mipmap-anydpi-v26/ic_launcher.xml
"""

from __future__ import annotations

import math
import struct
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets"

GREEN = (46, 160, 67)  # same as State::Online in src/ui.rs
WHITE = (255, 255, 255)
SIZES = (16, 32, 48, 64, 128, 256)

# Android: launcher icons are 48 dp, an adaptive-icon foreground is 108 dp, and
# both are rasterised per density bucket.
ANDROID_RES = ROOT / "android/app/src/main/res"
DENSITIES = {"mdpi": 1.0, "hdpi": 1.5, "xhdpi": 2.0, "xxhdpi": 3.0, "xxxhdpi": 4.0}



def _png(size: int, background: bool = True) -> bytes:
    """Render one square icon as RGBA PNG bytes.

    `background` false draws the white letter alone on transparency, which is
    what an adaptive icon's foreground layer wants.
    """
    half = (size - 1) / 2.0
    radius = size * 0.47
    edge = max(size * 0.06, 1.0)

    rows = bytearray()
    for y in range(size):
        rows.append(0)  # PNG filter type: none
        for x in range(size):
            if not background:
                t = _letter_cover(x, y, size)
                rows.extend((WHITE[0], WHITE[1], WHITE[2], int(round(t * 255))))
                continue
            d = math.hypot(x - half, y - half)
            if d <= radius:
                alpha = 1.0
            elif d <= radius + edge:
                alpha = (radius + edge - d) / edge
            else:
                alpha = 0.0
            if alpha <= 0.0:
                rows.extend((0, 0, 0, 0))
                continue

            t = _letter_cover(x, y, size)
            rows.extend(
                (
                    _mix(GREEN[0], WHITE[0], t),
                    _mix(GREEN[1], WHITE[1], t),
                    _mix(GREEN[2], WHITE[2], t),
                    int(round(alpha * 255)),
                )
            )

    def chunk(tag: bytes, data: bytes) -> bytes:
        body = tag + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", ihdr)
        + chunk(b"IDAT", zlib.compress(bytes(rows), 9))
        + chunk(b"IEND", b"")
    )


def _mix(a: int, b: int, t: float) -> int:
    return int(round(a + (b - a) * t))


def _letter_cover(x: float, y: float, size: float) -> float:
    """Coverage of the letter "T" at a pixel centre, 0.0 .. 1.0."""
    half = (size - 1) / 2.0
    bar_half_w = size * 0.17
    bar_top = size * 0.29
    bar_thick = max(size * 0.115, 1.2)
    stem_half = max(size * 0.062, 0.9)
    stem_bottom = size * 0.74
    feather = 0.75  # ~1px anti-aliased border

    inside_bar = abs(x - half) <= bar_half_w and bar_top <= y <= bar_top + bar_thick
    inside_stem = abs(x - half) <= stem_half and bar_top <= y <= stem_bottom
    if inside_bar or inside_stem:
        return 1.0

    # Distance to the nearest edge, so diagonal pixels blend instead of stair-stepping.
    dx_bar = abs(x - half) - bar_half_w
    dy_bar = max(bar_top - y, y - (bar_top + bar_thick))
    dx_stem = abs(x - half) - stem_half
    dy_stem = max(bar_top - y, y - stem_bottom)
    gap = min(max(dx_bar, dy_bar), max(dx_stem, dy_stem))
    if gap <= 0:
        return 1.0
    return max(0.0, 1.0 - gap / feather)


def _ico(pngs: list[tuple[int, bytes]]) -> bytes:
    """Wrap PNG images in an ICO container (Windows Vista and later)."""
    header = struct.pack("<HHH", 0, 1, len(pngs))
    offset = 6 + 16 * len(pngs)
    entries = bytearray()
    for size, data in pngs:
        dim = 0 if size >= 256 else size
        entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    return header + bytes(entries) + b"".join(data for _, data in pngs)


def _android_icons() -> None:
    """Write the launcher icon in every density plus the adaptive icon."""
    for density, scale in DENSITIES.items():
        folder = ANDROID_RES / f"mipmap-{density}"
        folder.mkdir(parents=True, exist_ok=True)
        (folder / "ic_launcher.png").write_bytes(_png(round(48 * scale)))
        (folder / "ic_launcher_foreground.png").write_bytes(
            _png(round(108 * scale), background=False)
        )

    values = ANDROID_RES / "values"
    values.mkdir(parents=True, exist_ok=True)
    (values / "colors.xml").write_text(
        '<?xml version="1.0" encoding="utf-8"?>\n'
        "<resources>\n"
        '  <color name="ic_launcher_background">#2ea043</color>\n'
        "</resources>\n",
        encoding="utf-8",
    )

    anydpi = ANDROID_RES / "mipmap-anydpi-v26"
    anydpi.mkdir(parents=True, exist_ok=True)
    (anydpi / "ic_launcher.xml").write_text(
        '<?xml version="1.0" encoding="utf-8"?>\n'
        '<adaptive-icon xmlns:android="http://schemas.android.com/apk/res/android">\n'
        '  <background android:drawable="@color/ic_launcher_background" />\n'
        '  <foreground android:drawable="@mipmap/ic_launcher_foreground" />\n'
        "</adaptive-icon>\n",
        encoding="utf-8",
    )


def main() -> None:
    ASSETS.mkdir(parents=True, exist_ok=True)
    pngs = [(size, _png(size)) for size in SIZES]
    (ASSETS / "thunet.ico").write_bytes(_ico(pngs))
    (ASSETS / "thunet.png").write_bytes(dict(pngs)[256])
    _android_icons()
    print(f"wrote {ASSETS / 'thunet.ico'} ({len(SIZES)} sizes)")
    print(f"wrote {ANDROID_RES}/mipmap-* (launcher and adaptive foreground)")


if __name__ == "__main__":
    main()
