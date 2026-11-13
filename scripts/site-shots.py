"""Publish the light-theme interface snapshots to the promo site.

The site shows the app on a light surface, so it takes the `-light` captures from
`tests/snapshots/`, trims the window shadow off every edge and cuts the picture
down to the part worth looking at: a whole window is mostly empty surface, and a
site section with a blank white rectangle in it looks broken.

Run it after regenerating the snapshots (`VB_SNAPSHOT`), before packaging the
site. It fails loudly when a source capture is missing, so a release cannot ship
with a stale or partial screenshot.

    python scripts/site-shots.py
"""
from __future__ import annotations

import sys
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "tests" / "snapshots"
TARGET = ROOT / "site" / "assets"

# Pixels removed from every side: the capture includes the window shadow, which
# reads as a dark stripe on the light page. 14 = the original 8 plus 6 more the
# owner asked for, so no shadow edge survives on the light surface.
TRIM = 14
# Space kept around the content after the smart crop.
MARGIN = 24
# A capture pixel this dark is the shadow, not content.
SHADOW = 40
# How far a pixel may be from the window surface and still count as empty.
TOLERANCE = 10

# source capture (light theme) -> the name the site pages use
MAP: dict[str, str] = {
    "capture-recording-light.png": "app-record.png",
    "history-expanded-light.png": "app-history.png",
    "settings-light.png": "app-settings.png",
    "about-light.png": "app-about.png",
    "phone-light.png": "app-phone.png",
}


def trim_shadow(image: Image.Image) -> Image.Image:
    width, height = image.size
    if width <= 2 * TRIM or height <= 2 * TRIM:
        raise SystemExit(f"snapshot too small to trim {TRIM}px: {width}x{height}")
    return image.crop((TRIM, TRIM, width - TRIM, height - TRIM))


def surface_colour(image: Image.Image) -> tuple[int, int, int]:
    """The window's own background: the most common colour in the capture."""
    counts: dict[tuple[int, int, int], int] = {}
    step = 4
    for y in range(0, image.height, step):
        for x in range(0, image.width, step):
            pixel = image.getpixel((x, y))
            if pixel[3] < 250:
                continue
            key = (pixel[0], pixel[1], pixel[2])
            counts[key] = counts.get(key, 0) + 1
    return max(counts, key=counts.get) if counts else (255, 255, 255)


def crop_to_content(image: Image.Image) -> Image.Image:
    """Cut the empty surface off the bottom and the right edge."""
    surface = surface_colour(image)
    rgb = image.convert("RGB")
    width, height = rgb.size
    surface_image = Image.new("RGB", (1, 1), surface)
    del surface_image

    def empty(x: int, y: int) -> bool:
        pixel = rgb.getpixel((x, y))
        if max(pixel) < SHADOW:
            return True
        return all(abs(pixel[i] - surface[i]) <= TOLERANCE for i in range(3))

    bottom = height
    while bottom > MARGIN and all(empty(x, bottom - 1) for x in range(0, width, 6)):
        bottom -= 1
    right = width
    while right > MARGIN and all(empty(right - 1, y) for y in range(0, bottom, 6)):
        right -= 1
    bottom = min(height, bottom + MARGIN)
    right = min(width, right + MARGIN)
    return image.crop((0, 0, right, bottom))


def crop_around_modal(image: Image.Image) -> Image.Image:
    """The About window sits on a dimmed backdrop; keep the bright panel only."""
    rgb = image.convert("RGB")
    width, height = rgb.size
    left, top, right, bottom = width, height, 0, 0
    # The operating system title bar is bright too, so start below it.
    skip = 64
    for y in range(skip, height, 2):
        for x in range(0, width, 2):
            r, g, b = rgb.getpixel((x, y))
            if r > 246 and g > 246 and b > 246:
                left = min(left, x)
                top = min(top, y)
                right = max(right, x)
                bottom = max(bottom, y)
    if right <= left or bottom <= top:
        return crop_to_content(image)
    left = max(0, left - MARGIN)
    top = max(0, top - MARGIN)
    right = min(width, right + MARGIN)
    bottom = min(height, bottom + MARGIN)
    return image.crop((left, top, right, bottom))


def main() -> int:
    if not TARGET.is_dir():
        TARGET.mkdir(parents=True)
    sizes: dict[str, tuple[int, int]] = {}
    for source_name, target_name in MAP.items():
        source = SOURCE / source_name
        if not source.is_file():
            raise SystemExit(f"missing light snapshot: {source} (regenerate the snapshots first)")
        with Image.open(source) as opened:
            image = trim_shadow(opened.convert("RGBA"))
        if target_name == "app-about.png":
            image = crop_around_modal(image)
        else:
            image = crop_to_content(image)
        image.convert("RGB").save(TARGET / target_name, "PNG", optimize=True)
        sizes[target_name] = (image.width, image.height)
        print(f"{source_name:30s} -> {target_name:20s} {image.width}x{image.height}")

    print(f"published {len(sizes)} screenshots into {TARGET}")
    for name, (width, height) in sorted(sizes.items()):
        print(f"  {name}: {width}x{height}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
