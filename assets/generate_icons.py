#!/usr/bin/env python3
"""Regenerate original branding with installed Inkscape + Pillow, or validate it.

Maintainer-only command: python assets/generate_icons.py
Validation (no renderer required): python assets/generate_icons.py --check
No downloads, fonts, UI/runtime dependencies, or global configuration changes.
"""
from __future__ import annotations

import argparse
import os
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent
SIZES = (16, 20, 24, 32, 40, 48, 64, 128, 256)
NAME = "gloamere-image"


def validate() -> None:
    for size in SIZES:
        with Image.open(ROOT / f"{NAME}-{size}.png") as frame:
            frame.load()
            assert frame.size == (size, size), (size, frame.size)
            assert frame.mode == "RGBA", (size, frame.mode)
            assert frame.getextrema()[3] == (0, 255), f"{size}: missing alpha/opaque pixels"
            assert frame.getpixel((0, 0))[3] == 0, f"{size}: opaque corner"
            assert frame.getpixel((size // 2, size // 2))[3] == 255
    data = (ROOT / f"{NAME}.ico").read_bytes()
    assert struct.unpack_from("<HHH", data) == (0, 1, len(SIZES))
    seen = set()
    with Image.open(ROOT / f"{NAME}.ico") as icon:
        assert icon.ico.sizes() == {(n, n) for n in SIZES}
        for index in range(len(SIZES)):
            w, h, colors, reserved, planes, depth, length, offset = struct.unpack_from(
                "<BBBBHHII", data, 6 + index * 16
            )
            size = w or 256
            assert size == (h or 256) and size in SIZES and size not in seen
            assert colors == reserved == 0 and planes in (0, 1) and depth == 32
            assert offset >= 6 + 16 * len(SIZES) and offset + length <= len(data)
            assert data[offset:offset + 8] == b"\x89PNG\r\n\x1a\n"
            seen.add(size)
            actual = icon.ico.getimage((size, size)).convert("RGBA")
            with Image.open(ROOT / f"{NAME}-{size}.png") as expected:
                assert actual.tobytes() == expected.convert("RGBA").tobytes(), size
    assert seen == set(SIZES)
    print(f"Validated RGBA PNGs and {len(SIZES)} exact ICO frames: {SIZES}; ICO {len(data):,} bytes")


def generate() -> None:
    inkscape = shutil.which("inkscape")
    if not inkscape:
        raise SystemExit("Install official Inkscape to regenerate; committed assets already support builds.")
    frames = []
    with tempfile.TemporaryDirectory(prefix="gloamere-icons-") as directory:
        temporary = Path(directory)
        environment = os.environ.copy()
        # Do not make or alter the user's/global Inkscape profile.
        environment["INKSCAPE_PROFILE_DIR"] = str(temporary / "inkscape-profile")
        for size in SIZES:
            destination = temporary / f"{size}.png"
            subprocess.run([
                inkscape, str(ROOT / f"{NAME}.svg"), "--export-area-page",
                "--export-background-opacity=0", f"--export-width={size}",
                f"--export-height={size}", f"--export-filename={destination}",
            ], check=True, env=environment, capture_output=True, text=True)
            with Image.open(destination) as rendered:
                rgba = rendered.convert("RGBA")
                # Fresh image strips renderer metadata, including dates/profile paths.
                frame = Image.frombytes("RGBA", rgba.size, rgba.tobytes())
                frame.save(ROOT / f"{NAME}-{size}.png", optimize=False, compress_level=9)
                frames.append(frame)
    frames[-1].save(ROOT / f"{NAME}.ico", format="ICO", sizes=[(n, n) for n in SIZES],
                    append_images=frames[:-1])
    # Each row includes actual-size images, followed by a nearest-neighbor 16px
    # enlargement. This is evidence for inspection, not an app runtime asset.
    sheet = Image.new("RGB", (1100, 660), "#F5F4EF")
    draw = ImageDraw.Draw(sheet)
    for row, (background, foreground) in enumerate((("#F5F4EF", "#113E42"), ("#182225", "#E2ECE5"))):
        y = row * 330
        draw.rectangle((0, y, 1100, y + 330), fill=background)
        draw.text((20, y + 16), f"gloamere-image  |  {'light' if row == 0 else 'dark'} background", fill=foreground)
        x = 20
        for frame in frames:
            n = frame.width
            sheet.paste(frame, (x, y + 165 - n // 2), frame)
            draw.text((x, y + 305), f"{n}px", fill=foreground)
            x += max(n + 18, 47)
        zoom = frames[0].resize((96, 96), Image.Resampling.NEAREST)
        sheet.paste(zoom, (980, y + 117), zoom)
        draw.text((980, y + 225), "16px x6", fill=foreground)
    sheet.save(ROOT / f"{NAME}-preview.png", compress_level=9)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="validate without rendering or modifying assets")
    args = parser.parse_args()
    if not args.check:
        generate()
    validate()
