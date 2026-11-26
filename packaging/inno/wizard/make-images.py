"""Draws the installer wizard images from the NopBar app icon.

    python packaging/inno/wizard/make-images.py

Writes, next to this file, one PNG per DPI step (Inno Setup 6.6+ image
areas, see nopbar.iss) for the light and the dark wizard:

    large-<light|dark>-<w>x<h>.png   Welcome / Finished left panel
    small-<light|dark>-<n>.png       top-right corner of the inner pages

Both are transparent (WizardImageBackColor=none), so they sit on the wizard's
own background (WizardBackColor / ...DynamicDark) with no box edge. The large
one uses a faint round tone behind the icon. The small one is the icon alone.
Needs Pillow.
"""

import math
from pathlib import Path

from PIL import Image

HERE = Path(__file__).resolve().parent
ICON = HERE.parents[2] / "assets" / "brand" / "icon-256.png"

# Image areas per DPI (100 125 150 175 200 225 250 %), from the
# WizardImageFile / WizardSmallImageFile help pages of Inno Setup 6.7.
LARGE = [(202, 386), (269, 515), (336, 643), (403, 772), (430, 824), (498, 953), (534, 1022)]
SMALL = [58, 77, 97, 116, 124, 143, 159]

# Per wizard look: the background tone's color and peak alpha.
LOOKS = {
    "light": ((24, 24, 24), 10),
    "dark": ((236, 236, 236), 9),
}

def large(w, h, icon, tone_rgb, tone):
    # The faint round tone: a soft falloff centred on the icon, zero well
    # before the edges (so nothing shows at the panel's border).
    cx, cy = w / 2, h * 0.44
    r = w * 0.62
    alpha = Image.new("L", (w, h), 0)
    px = alpha.load()
    for y in range(h):
        for x in range(w):
            q = math.hypot(x + 0.5 - cx, y + 0.5 - cy) / r
            if q < 1:
                px[x, y] = round(tone * (0.5 + 0.5 * math.cos(math.pi * q)))
    img = Image.new("RGBA", (w, h), tone_rgb + (0,))
    img.putalpha(alpha)
    # The visible face spans about 30% of the panel's width.
    side = round(w * 0.34)
    resized = icon.resize((side, side), Image.Resampling.LANCZOS)
    img.alpha_composite(resized, (round(cx - side / 2), round(cy - side / 2)))
    return img


def small(n, icon):
    img = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    side = round(n * 0.84)
    resized = icon.resize((side, side), Image.Resampling.LANCZOS)
    pad = (n - side) // 2
    img.alpha_composite(resized, (pad, pad))
    return img


def main():
    with Image.open(ICON) as source:
        icon = source.convert("RGBA")
    paths = []
    for name, (tone_rgb, tone) in LOOKS.items():
        for w, h in LARGE:
            p = HERE / f"large-{name}-{w}x{h}.png"
            large(w, h, icon, tone_rgb, tone).save(p, optimize=True)
            paths.append(p)
        for n in SMALL:
            p = HERE / f"small-{name}-{n}.png"
            small(n, icon).save(p, optimize=True)
            paths.append(p)
    for p in sorted(paths):
        print(f"{p.stat().st_size:>7}  {p.name}")
    total = sum(p.stat().st_size for p in paths)
    print(f"{total:>7}  total")


if __name__ == "__main__":
    main()
