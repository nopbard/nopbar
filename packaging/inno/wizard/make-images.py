"""Draws the installer wizard images: the gnome mark in its two brand colors
(scripts/brand-icons.py: the paths and the colors).

    python packaging/inno/wizard/make-images.py

Writes, next to this file, one PNG per DPI step (Inno Setup 6.6+ image
areas, see nopbar.iss) for the light and the dark wizard:

    large-<light|dark>-<w>x<h>.png   Welcome / Finished left panel
    small-<light|dark>-<n>.png       top-right corner of the inner pages

Both are transparent (WizardImageBackColor=none), so they sit on the wizard's
own background (WizardBackColor / ...DynamicDark) with no box edge. The large
one is the gnome mark with a very faint round tone behind it; the small one is
the mark alone. Needs Pillow.
"""

import importlib.util
import math
from pathlib import Path

from PIL import Image

HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("brand_icons", HERE.parents[2] / "scripts" / "brand-icons.py")
brand = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(brand)

# Image areas per DPI (100 125 150 175 200 225 250 %), from the
# WizardImageFile / WizardSmallImageFile help pages of Inno Setup 6.7.
LARGE = [(202, 386), (269, 515), (336, 643), (403, 772), (430, 824), (498, 953), (534, 1022)]
SMALL = [58, 77, 97, 116, 124, 143, 159]

# Per wizard look: the mark's palette (brand MARK: light grounds get the
# dark beard), the tone's colour and its peak alpha.
LOOKS = {
    "light": {"mark": "light", "tone_rgb": (24, 24, 24), "tone": 10},
    "dark": {"mark": "dark", "tone_rgb": (236, 236, 236), "tone": 9},
}

SS = 8  # supersampling for the mark's edges

MW, MH = brand.MX1 - brand.MX0, brand.MY1 - brand.MY0


def mark(height, look):
    """The mark, `height` px tall, in its two colours (its own width)."""
    w = max(1, math.ceil(MW * height / MH))
    k = height / MH
    side = max(w, height)
    canvas = Image.new("RGBA", (side, side), (0, 0, 0, 0))
    fn = lambda q: ((q[0] - brand.MX0) * k, (q[1] - brand.MY0) * k)
    for part in ("hat", "beard"):
        m = brand.mask(brand.PARTS[part], side, fn, SS)
        y0, y1 = brand.part_box(fn, part)
        brand.fill(canvas, m, brand.gradient(side, *brand.MARK[look["mark"]][part], y0, y1))
    return canvas.crop((0, 0, w, height))


def large(w, h, look):
    # The faint round tone: a soft falloff centred on the mark, zero well
    # before the edges (so nothing shows at the panel's border).
    cx, cy = w / 2, h * 0.44
    r = w * 0.62
    alpha = Image.new("L", (w, h), 0)
    px = alpha.load()
    for y in range(h):
        for x in range(w):
            q = math.hypot(x + 0.5 - cx, y + 0.5 - cy) / r
            if q < 1:
                px[x, y] = round(look["tone"] * (0.5 + 0.5 * math.cos(math.pi * q)))
    img = Image.new("RGBA", (w, h), look["tone_rgb"] + (0,))
    img.putalpha(alpha)
    # The mark: about 30% of the panel's width, a little above the middle.
    mh = round(w * 0.30 * MH / MW)
    m = mark(mh, look)
    img.alpha_composite(m, (round(cx - m.width / 2), round(cy - mh / 2)))
    return img


def small(n, look):
    img = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    mh = round(n * 0.78)
    m = mark(mh, look)
    img.alpha_composite(m, ((n - m.width) // 2, (n - mh) // 2))
    return img


def main():
    for old in HERE.glob("*.png"):
        old.unlink()
    total = 0
    for name, look in LOOKS.items():
        for w, h in LARGE:
            p = HERE / f"large-{name}-{w}x{h}.png"
            large(w, h, look).save(p, optimize=True)
            total += p.stat().st_size
        for n in SMALL:
            p = HERE / f"small-{name}-{n}.png"
            small(n, look).save(p, optimize=True)
            total += p.stat().st_size
    for p in sorted(HERE.glob("*.png")):
        print(f"{p.stat().st_size:>7}  {p.name}")
    print(f"{total:>7}  total")


if __name__ == "__main__":
    main()
