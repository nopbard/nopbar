"""Draws NopBar's brand pictures from one definition (the gnome mark in its
two colors, 2026-11-11: a brick hat and a grey beard, the dock icon mockup's
style C, docs/design/icons/mockup.html PAL.C / ART.mark).

    python scripts/brand-icons.py [--preview DIR]

Writes (all under assets/brand unless said):

    mark.svg, mark-dark.svg      the mark alone, for light / dark grounds
    icon.svg, icon-light.svg     the app icon (graphite / white squircle)
    icon-small.svg               the app icon as drawn at 16-32 px
    icon-1024.png, icon-256.png, icon-light-1024.png, icon-light-256.png
    nopbar.ico, nopbar-light.ico 16 20 24 32 40 48 64 128 256
    dock/res/nopbar.ico          = nopbar.ico (baked into nopbar.exe)
    tray/tray-white-<n>.png      the mark for a dark taskbar (16..48)
    tray/tray-black-<n>.png      the mark for a light taskbar

("white" / "black" keep their old names: the mark is no longer one colour,
but those are still the dark- and light-taskbar sets dock/src/dock/trayicon.rs
picks from.)

--preview DIR also writes contact sheets there (enlarged tray marks on both
taskbars, the icons at every size). Needs Pillow only.
"""

import math
import struct
import sys
from io import BytesIO
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFilter

ROOT = Path(__file__).resolve().parents[1]
BRAND = ROOT / "assets" / "brand"

# ---- the definition -----------------------------------------------------------

# The mark in its 1024 box (assets/brand/mark.svg): the hat (with the nose
# under its brim) and the beard.
HAT = ("M251 558 C238 552 246 534 256 512 C298 426 362 336 422 242 C486 144 560 98 636 98 "
       "C712 98 768 138 782 198 C796 248 772 280 748 281 C738 281 735 275 742 264 "
       "C758 238 718 190 678 190 C636 190 602 218 588 254 C576 294 596 330 618 362 "
       "C652 420 690 488 704 530 C710 544 712 552 702 558 C650 522 590 505 530 502 "
       "C528 534 512 549 482 549 C452 549 438 532 437 502 C370 505 300 524 251 558 Z")
BEARD = ("M264 590 C270 658 364 712 478 712 C592 712 686 658 694 590 C718 610 724 648 712 696 "
         "C684 778 608 854 508 914 C490 927 478 932 460 922 C366 874 276 794 248 720 "
         "C228 666 234 614 264 590 Z")

# Top and bottom of each part's vertical gradient (over the part's own box),
# by the ground the mark sits on. The beard follows the mockup's ink.
MARK = {
    "light": {"hat": ("#C97B6E", "#A0534A"), "beard": ("#696A6F", "#2F3033")},
    "dark": {"hat": ("#D88A7C", "#B0615A"), "beard": ("#F8F8F9", "#C6C7CA")},
}
# The app icon's plate (the setup window's icon_top / icon_bottom too).
PLATE = {"dark": ("#3A3A3A", "#242424"), "light": ("#FFFFFF", "#ECECEB")}
# The mockup's shadow filter ("sh"), per look: the soft drop (A), the
# contact shadow (C) and the white top-edge light (edge); in the mockup's
# units (a 100-unit tile).
SHADE = {
    "light": {"shA": 0.26, "shC": 0.22, "edge": 0.5, "plate": 0.27},
    "dark": {"shA": 0.5, "shC": 0.42, "edge": 0.35, "plate": 0.4},
}

# The big icon in its 1024 box: the squircle plate (x 100..924, y 94..918,
# as shipped since 0.1.3x) with its drop shadow; the mark at 0.7 centred on
# the plate.
BIG = {"plate": (100.0, 94.0, 924.0, 918.0), "mark": 0.7}
# The small icon (16..32 px): the plate to a 1/16 margin, the mark as large
# on it; no plate shadow (no room), the contact shadow only.
SMALL_MAX = 32
SMALL_MARK = 1.12  # the mark this much larger on the small plate
ICO_SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]
TRAY_SIZES = [16, 20, 24, 32, 40, 48]
# Tray: the mark's height and top (px), by side: about as tall as the old
# hinted set, centred once the gap row is in (tray()).
TRAY_FIT = {16: (13, 1.0), 20: (17, 1.2), 24: (21, 1.0), 32: (28, 2.0), 40: (35, 2.2), 48: (43, 1.8)}
# Up to this side the tray mark is the simple one: a flat brim (the nose
# and the dips beside it are under a pixel) and crisper edges.
TRAY_SIMPLE = 24
BRIM = [(251.0, 558.0), (437.0, 500.0), (530.0, 500.0), (702.0, 558.0)]

# ---- geometry -----------------------------------------------------------------


def parse(d):
    """Absolute M / C / Z path data as closed polygons (points in its box)."""
    toks = d.replace("M", " M ").replace("C", " C ").replace("Z", " Z ").split()
    polys, cur, pos, i = [], [], (0.0, 0.0), 0
    while i < len(toks):
        t = toks[i]
        if t == "M":
            pos = (float(toks[i + 1]), float(toks[i + 2]))
            cur = [pos]
            i += 3
        elif t == "C":
            i += 1
            while i < len(toks) and toks[i] not in ("M", "C", "Z"):
                p1 = (float(toks[i]), float(toks[i + 1]))
                p2 = (float(toks[i + 2]), float(toks[i + 3]))
                p3 = (float(toks[i + 4]), float(toks[i + 5]))
                for k in range(1, 33):
                    u = k / 32
                    a, b, c, e = (1 - u) ** 3, 3 * (1 - u) ** 2 * u, 3 * (1 - u) * u * u, u ** 3
                    cur.append((a * pos[0] + b * p1[0] + c * p2[0] + e * p3[0],
                                a * pos[1] + b * p1[1] + c * p2[1] + e * p3[1]))
                pos = p3
                i += 6
        elif t == "Z":
            polys.append(cur)
            cur = []
            i += 1
        else:
            raise ValueError(f"unexpected token {t!r}")
    return polys


PARTS = {"hat": parse(HAT), "beard": parse(BEARD)}
ALL = [q for ps in PARTS.values() for p in ps for q in p]
MX0, MY0 = min(x for x, _ in ALL), min(y for _, y in ALL)
MX1, MY1 = max(x for x, _ in ALL), max(y for _, y in ALL)
MCX, MCY = (MX0 + MX1) / 2, (MY0 + MY1) / 2


def squircle(l, t, r, b, n=5.0, steps=240):
    """The dock's superellipse (mockup SQ) in the box."""
    cx, cy, hw, hh = (l + r) / 2, (t + b) / 2, (r - l) / 2, (b - t) / 2
    pts = []
    for i in range(steps):
        a = i / steps * 2 * math.pi
        c, s = math.cos(a), math.sin(a)
        pts.append((cx + hw * math.copysign(abs(c) ** (2 / n), c), cy + hh * math.copysign(abs(s) ** (2 / n), s)))
    return pts


def hexrgb(h):
    h = h.lstrip("#")
    return tuple(int(h[i:i + 2], 16) for i in (0, 2, 4))


# ---- raster -------------------------------------------------------------------


def mask(polys, size, fn, ss):
    """Polygons mapped by fn (box -> px) as an antialiased L mask."""
    big = Image.new("L", (size * ss, size * ss), 0)
    dr = ImageDraw.Draw(big)
    for p in polys:
        dr.polygon([(fn(q)[0] * ss, fn(q)[1] * ss) for q in p], fill=255)
    return big.reduce(ss)


def gradient(size, top, bottom, y0, y1):
    """A vertical gradient image, `top` at y0 to `bottom` at y1 (px)."""
    t, b = hexrgb(top), hexrgb(bottom)
    col = Image.new("RGB", (1, size))
    for y in range(size):
        u = min(1.0, max(0.0, (y + 0.5 - y0) / max(1e-6, y1 - y0)))
        col.putpixel((0, y), tuple(round(t[k] + (b[k] - t[k]) * u) for k in range(3)))
    return col.resize((size, size), Image.NEAREST)


def fill(canvas, m, rgb_img):
    layer = rgb_img.convert("RGBA")
    layer.putalpha(m)
    canvas.alpha_composite(layer)


def shadow(canvas, m, sigma, dy, opacity, rgb=(0, 0, 0)):
    """A blurred copy of mask m moved down dy, at opacity, under what
    comes next."""
    if opacity <= 0:
        return
    a = m.filter(ImageFilter.GaussianBlur(sigma)) if sigma > 0.05 else m.copy()
    shifted = Image.new("L", a.size, 0)
    shifted.paste(a, (0, round(dy)))
    shifted = shifted.point(lambda v: round(v * opacity))
    layer = Image.new("RGBA", a.size, rgb + (0,))
    layer.putalpha(shifted)
    canvas.alpha_composite(layer)


def part_box(fn, part):
    ys = [fn(q)[1] for p in PARTS[part] for q in p]
    return min(ys), max(ys)


def draw_mark(canvas, fn, look, ss, unit, effects=True, sharpen=0.0):
    """The mark (fn: its 1024 box -> px) in `look`'s colors, with the
    mockup's shadow and top light when effects; `unit` is px per mockup
    unit."""
    size = canvas.size[0]
    masks = {k: mask(PARTS[k], size, fn, ss) for k in PARTS}
    if sharpen:
        masks = {k: m.point(lambda v: max(0, min(255, round((v - 128) * sharpen + 128)))) for k, m in masks.items()}
    union = ImageChops.lighter(masks["hat"], masks["beard"])
    sh = SHADE[look]
    if effects:
        shadow(canvas, union, 2.2 * unit, 2.4 * unit, sh["shA"])
        shadow(canvas, union, 0.55 * unit, 0.7 * unit, sh["shC"])
    for k in ("hat", "beard"):
        y0, y1 = part_box(fn, k)
        top, bottom = MARK[look][k]
        fill(canvas, masks[k], gradient(size, top, bottom, y0, y1))
    if effects and 0.9 * unit >= 0.35:
        dn = Image.new("L", union.size, 0)
        dn.paste(union, (0, round(0.9 * unit)))
        rim = ImageChops.subtract(union, dn).point(lambda v: round(v * sh["edge"]))
        layer = Image.new("RGBA", union.size, (255, 255, 255, 0))
        layer.putalpha(rim)
        canvas.alpha_composite(layer)


def icon(size, look):
    """The app icon at `size` px."""
    ss = 4 if size >= 512 else 8 if size >= 128 else 16
    c = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    if size <= SMALL_MAX:
        m = max(1, round(size / 16))
        l, t, r, b = m, m, size - m, size - m
        plate_shadow = False
    else:
        k = size / 1024
        l, t, r, b = (v * k for v in BIG["plate"])
        plate_shadow = True
    side = r - l
    plate = squircle(l, t, r, b)
    pm = mask([plate], size, lambda q: q, ss)
    if plate_shadow:
        shadow(c, pm, 18 * size / 1024, 8 * size / 1024, SHADE[look]["plate"])
    fill(c, pm, gradient(size, *PLATE[look], t, b))
    # The mark: 0.7 of the 1024 box on an 824 plate, centred on the plate.
    s = BIG["mark"] * side / 824 * (SMALL_MARK if size <= SMALL_MAX else 1.0)
    cx, cy = (l + r) / 2, (t + b) / 2
    fn = lambda q: (cx + (q[0] - 512) * s, cy + (q[1] - MCY) * s)
    unit = side / 100
    draw_mark(c, fn, look, ss, unit, effects=True)
    # Nothing outside the plate but its own shadow.
    return c


def tray(size, look):
    """The mark alone for the notification area, hinted: the hat's brim and
    the beard's top on whole pixel rows with a clear row between (two
    colours that would otherwise blend into one muddy row)."""
    h, top = TRAY_FIT[size]
    k = h / (MY1 - MY0)
    brim, chin = 558.0, 590.0  # the hat's lowest point, the beard's highest
    yb = round(top + (brim - MY0) * k)
    gap = max(1, round((chin - brim) * k))
    hat_dy = yb - (top + (brim - MY0) * k)
    beard_dy = (yb + gap) - (top + (chin - MY0) * k)
    # Centre on the beard's box, its left edge on a whole pixel.
    bx0 = min(q[0] for p in PARTS["beard"] for q in p)
    x0 = size / 2 - (512 - bx0) * k
    dx = round(x0) - x0
    c = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    ss = 16
    simple = size <= TRAY_SIMPLE
    sharpen = 1.15 if simple else 1.0
    for part, dy in (("hat", hat_dy), ("beard", beard_dy)):
        fn = lambda q, dy=dy: (size / 2 + (q[0] - 512) * k + dx, top + (q[1] - MY0) * k + dy)
        polys = PARTS[part] + ([BRIM] if simple and part == "hat" else [])
        m = mask(polys, size, fn, ss).point(lambda v: max(0, min(255, round((v - 128) * sharpen + 128))))
        y0, y1 = part_box(fn, part)
        fill(c, m, gradient(size, *MARK[look][part], y0, y1))
    return c


# ---- files --------------------------------------------------------------------


def png_bytes(img):
    b = BytesIO()
    img.save(b, "PNG", optimize=True)
    return b.getvalue()


def bmp_entry(img):
    """A 32-bit DIB icon entry (BGRA, bottom-up, with its AND mask)."""
    w, h = img.size
    hdr = struct.pack("<IiiHHIIiiII", 40, w, h * 2, 1, 32, 0, 0, 0, 0, 0, 0)
    px = img.load()
    rows = bytearray()
    for y in range(h - 1, -1, -1):
        for x in range(w):
            r, g, b, a = px[x, y]
            rows += bytes((b, g, r, a))
    stride = ((w + 31) // 32) * 4
    andm = bytearray()
    for y in range(h - 1, -1, -1):
        bits = bytearray(stride)
        for x in range(w):
            if px[x, y][3] == 0:
                bits[x // 8] |= 0x80 >> (x % 8)
        andm += bits
    return hdr + bytes(rows) + bytes(andm)


def write_ico(path, imgs):
    data = [png_bytes(i) if i.size[0] >= 128 else bmp_entry(i) for i in imgs]
    out = struct.pack("<HHH", 0, 1, len(imgs))
    off = 6 + 16 * len(imgs)
    for img, d in zip(imgs, data):
        w = img.size[0]
        out += struct.pack("<BBBBHHII", w % 256, w % 256, 0, 0, 1, 32, len(d), off)
        off += len(d)
    path.write_bytes(out + b"".join(data))


def svg_grad(id_, top, bottom):
    return f'<linearGradient id="{id_}" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="{top}"/><stop offset="1" stop-color="{bottom}"/></linearGradient>'


def svg_mark_filter(id_, look, unit):
    sh = SHADE[look]
    f = lambda v: f"{v:.2f}".rstrip("0").rstrip(".")
    return (f'<filter id="{id_}" x="-30%" y="-30%" width="160%" height="170%" color-interpolation-filters="sRGB">'
            f'<feGaussianBlur in="SourceAlpha" stdDeviation="{f(2.2 * unit)}"/><feOffset dy="{f(2.4 * unit)}" result="o1"/>'
            f'<feComponentTransfer in="o1" result="s1"><feFuncA type="linear" slope="{sh["shA"]}"/></feComponentTransfer>'
            f'<feGaussianBlur in="SourceAlpha" stdDeviation="{f(0.55 * unit)}"/><feOffset dy="{f(0.7 * unit)}" result="o2"/>'
            f'<feComponentTransfer in="o2" result="s2"><feFuncA type="linear" slope="{sh["shC"]}"/></feComponentTransfer>'
            f'<feOffset in="SourceAlpha" dy="{f(0.9 * unit)}" result="dn"/><feComposite in="SourceAlpha" in2="dn" operator="out" result="rimA"/>'
            f'<feFlood flood-color="#fff" flood-opacity="{sh["edge"]}"/><feComposite in2="rimA" operator="in" result="rimW"/>'
            f'<feMerge><feMergeNode in="s1"/><feMergeNode in="s2"/><feMergeNode in="SourceGraphic"/><feMergeNode in="rimW"/></feMerge></filter>')


def svg_path(pts):
    return "M" + "L".join(f"{x:.1f} {y:.1f}" for x, y in pts) + "Z"


def mark_svg(look):
    c = MARK[look]
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024"><defs>'
            f'{svg_grad("hat", *c["hat"])}{svg_grad("beard", *c["beard"])}</defs>'
            f'<path id="mark-hat" fill="url(#hat)" d="{HAT}"/>'
            f'<path id="mark-beard" fill="url(#beard)" d="{BEARD}"/></svg>\n')


def icon_svg(look, small=False):
    if small:
        l, t, r, b = 64.0, 64.0, 960.0, 960.0
    else:
        l, t, r, b = BIG["plate"]
    side = r - l
    s = BIG["mark"] * side / 824 * (SMALL_MARK if small else 1.0)
    cx, cy = (l + r) / 2, (t + b) / 2
    tx, ty = cx - 512 * s, cy - MCY * s
    c = MARK[look]
    defs = svg_grad("plate", *PLATE[look]) + svg_grad("hat", *c["hat"]) + svg_grad("beard", *c["beard"])
    defs += svg_mark_filter("sh", look, side / 100)
    plate = svg_path(squircle(l, t, r, b))
    out = f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024"><defs>{defs}'
    if not small:
        out += (f'<filter id="drop" x="-20%" y="-20%" width="140%" height="150%" color-interpolation-filters="sRGB">'
                f'<feDropShadow dy="8" stdDeviation="18" flood-color="#000" flood-opacity="{SHADE[look]["plate"]}"/></filter>')
    out += "</defs>"
    out += f'<path fill="url(#plate)"{"" if small else " filter=\"url(#drop)\""} d="{plate}"/>'
    out += (f'<g filter="url(#sh)"><g transform="translate({tx:.2f} {ty:.2f}) scale({s:.4f})">'
            f'<path fill="url(#hat)" d="{HAT}"/><path fill="url(#beard)" d="{BEARD}"/></g></g></svg>\n')
    return out


def main():
    preview = None
    if "--preview" in sys.argv:
        preview = Path(sys.argv[sys.argv.index("--preview") + 1])
        preview.mkdir(parents=True, exist_ok=True)
    (BRAND / "mark.svg").write_text(mark_svg("light"), encoding="utf-8")
    (BRAND / "mark-dark.svg").write_text(mark_svg("dark"), encoding="utf-8")
    (BRAND / "icon.svg").write_text(icon_svg("dark"), encoding="utf-8")
    (BRAND / "icon-light.svg").write_text(icon_svg("light"), encoding="utf-8")
    (BRAND / "icon-small.svg").write_text(icon_svg("dark", small=True), encoding="utf-8")

    icons = {look: {n: icon(n, look) for n in ICO_SIZES + [1024]} for look in ("dark", "light")}
    for look, stem in (("dark", "icon"), ("light", "icon-light")):
        for n in (1024, 256):
            icons[look][n].save(BRAND / f"{stem}-{n}.png", optimize=True)
    write_ico(BRAND / "nopbar.ico", [icons["dark"][n] for n in ICO_SIZES])
    write_ico(BRAND / "nopbar-light.ico", [icons["light"][n] for n in ICO_SIZES])
    (ROOT / "dock" / "res" / "nopbar.ico").write_bytes((BRAND / "nopbar.ico").read_bytes())
    for source, target in (
        ("icon-small.svg", "site/nopbar/img/icon-small.svg"),
        ("icon-256.png", "site/nopbar/img/icon-256.png"),
        ("nopbar.ico", "site/favicon.ico"),
    ):
        (ROOT / target).write_bytes((BRAND / source).read_bytes())


    trays = {}
    for name, look in (("white", "dark"), ("black", "light")):
        for n in TRAY_SIZES:
            img = tray(n, look)
            trays[(name, n)] = img
            img.save(BRAND / "tray" / f"tray-{name}-{n}.png", optimize=True)

    if preview:
        # Tray marks on Windows' dark and light taskbars, 1:1 and enlarged.
        cell = 112
        sheet = Image.new("RGBA", (len(TRAY_SIZES) * cell, 2 * cell + 2 * 64), (255, 255, 255, 255))
        for row, (name, ground) in enumerate((("white", (32, 32, 32)), ("black", (238, 238, 238)))):
            for i, n in enumerate(TRAY_SIZES):
                img = trays[(name, n)]
                k = max(1, 96 // n)
                big = img.resize((n * k, n * k), Image.NEAREST)
                tile = Image.new("RGBA", (cell, cell), ground + (255,))
                tile.alpha_composite(big, ((cell - n * k) // 2, (cell - n * k) // 2))
                sheet.paste(tile, (i * cell, row * cell))
                strip = Image.new("RGBA", (cell, 64), ground + (255,))
                strip.alpha_composite(img, ((cell - n) // 2, (64 - n) // 2))
                sheet.paste(strip, (i * cell, 2 * cell + row * 64))
        sheet.save(preview / "tray-sheet.png")
        # 16 px tray, 16x.
        for name, ground in (("white", (32, 32, 32)), ("black", (238, 238, 238))):
            t = Image.new("RGBA", (16, 16), ground + (255,))
            t.alpha_composite(trays[(name, 16)])
            t.resize((256, 256), Image.NEAREST).save(preview / f"tray16-{name}-x16.png")
        # Icons at the ICO sizes up to 128, on mid grey, light and dark grounds.
        sizes = [n for n in ICO_SIZES if n <= 128]
        w = sum(n + 16 for n in sizes) + 16
        sheet = Image.new("RGBA", (w, 3 * 290), (0, 0, 0, 255))
        for row, ground in enumerate(((128, 128, 128), (243, 243, 243), (32, 32, 32))):
            band = Image.new("RGBA", (w, 290), ground + (255,))
            for look_i, look in enumerate(("dark", "light")):
                xx = 16
                for n in sizes:
                    band.alpha_composite(icons[look][n], (xx, 8 + look_i * 140 + (128 - n) // 2))
                    xx += n + 16
            sheet.paste(band, (0, row * 290))
        sheet.save(preview / "icon-sizes.png")
        for look in ("dark", "light"):
            big = Image.new("RGBA", (1024, 1024), (128, 128, 128, 255))
            big.alpha_composite(icons[look][1024])
            big.resize((512, 512), Image.LANCZOS).save(preview / f"icon-{look}-512.png")
            small = Image.new("RGBA", (16 * 5 + 20 * 5 + 24 * 5 + 32 * 5 + 50, 32 * 5 + 20), (128, 128, 128, 255))
            x = 10
            for n in (16, 20, 24, 32):
                small.alpha_composite(icons[look][n].resize((n * 5, n * 5), Image.NEAREST), (x, 10))
                x += n * 5 + 10
            small.save(preview / f"icon-{look}-small-x5.png")
    print("brand icons written")


if __name__ == "__main__":
    main()
