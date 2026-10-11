"""Generates the launcher icons in res/ and the desktop window icon.

Run from this directory: python3 make_icons.py (needs Pillow).
"""
import os

from PIL import Image, ImageDraw

BACKGROUND = (32, 38, 52, 255)
COLORS = [(66, 133, 244), (52, 168, 83), (251, 188, 5), (234, 67, 53), (171, 71, 188)]


def draw_mark(draw, x0, y0, x1, y1, gap, radius, mono=False):
    """The treemap mark: five blocks filling the box (x0, y0)-(x1, y1)."""
    w, h = x1 - x0, y1 - y0
    fill = lambda i: (255, 255, 255, 255) if mono else COLORS[i] + (255,)
    a = x0 + w * 0.58
    b = y0 + h * 0.55
    c = a + gap / 2 + (x1 - a) * 0.5
    e = b + gap / 2 + (y1 - b) * 0.5
    boxes = [
        (x0, y0, a - gap / 2, y1),
        (a + gap / 2, y0, x1, b - gap / 2),
        (a + gap / 2, b + gap / 2, c - gap / 2, y1),
        (c + gap / 2, b + gap / 2, x1, e - gap / 2),
        (c + gap / 2, e + gap / 2, x1, y1),
    ]
    for i, box in enumerate(boxes):
        draw.rounded_rectangle(box, radius=radius * (1 if i < 2 else 0.7), fill=fill(i))


def legacy(size):
    s = size * 4  # supersample
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    d.rounded_rectangle([s * 0.03, s * 0.03, s * 0.97, s * 0.97], radius=s * 0.19, fill=BACKGROUND)
    p = s * 0.18
    draw_mark(d, p, p, s - p, s - p, gap=s * 0.03, radius=s * 0.045)
    return img.resize((size, size), Image.LANCZOS)


def layer(size, mono):
    # Adaptive icon layers are 108dp; only the middle 66dp is always visible.
    s = size * 4
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    lo, hi = s * 30 / 108, s * 78 / 108
    draw_mark(d, lo, lo, hi, hi, gap=s * 0.022, radius=s * 0.03, mono=mono)
    return img.resize((size, size), Image.LANCZOS)


DENSITIES = {"mdpi": 1.0, "hdpi": 1.5, "xhdpi": 2.0, "xxhdpi": 3.0, "xxxhdpi": 4.0}

for name, scale in DENSITIES.items():
    folder = f"res/mipmap-{name}"
    os.makedirs(folder, exist_ok=True)
    legacy(round(48 * scale)).save(f"{folder}/ic_launcher.png", optimize=True)
    layer(round(108 * scale), mono=False).save(f"{folder}/ic_launcher_foreground.png", optimize=True)
    layer(round(108 * scale), mono=True).save(f"{folder}/ic_launcher_monochrome.png", optimize=True)

legacy(256).save("../ui/icon.png", optimize=True)
print("icons written")
