"""Writes NopBar's gnome mark (two colors, 2026-11-11: a brick hat and a grey
beard, the dock icon mockup's style C, docs/design/icons/mockup.html PAL.C /
ART.mark) as SVG.

    python scripts/brand-icons.py

Writes, under assets/brand:

    mark.svg, mark-dark.svg      the mark alone, for light / dark grounds

The app icon is no longer drawn from the mark. Since 0.1.113 it is the
gnome's face (assets/brand/icon.svg, icon-small.svg, icon-*.png, nopbar.ico,
dock/res/nopbar.ico), drawn by hand with its own small sizes; since 0.1.116
the notification area uses those small sizes too (assets/brand/tray/tray-<n>.png)
with their outline alone for high contrast (tray-line-<n>.png). This script
does not write any of them.
"""

from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BRAND = ROOT / "assets" / "brand"

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


def svg_grad(id_, top, bottom):
    return f'<linearGradient id="{id_}" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="{top}"/><stop offset="1" stop-color="{bottom}"/></linearGradient>'


def mark_svg(look):
    c = MARK[look]
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024"><defs>'
            f'{svg_grad("hat", *c["hat"])}{svg_grad("beard", *c["beard"])}</defs>'
            f'<path id="mark-hat" fill="url(#hat)" d="{HAT}"/>'
            f'<path id="mark-beard" fill="url(#beard)" d="{BEARD}"/></svg>\n')


def main():
    (BRAND / "mark.svg").write_text(mark_svg("light"), encoding="utf-8")
    (BRAND / "mark-dark.svg").write_text(mark_svg("dark"), encoding="utf-8")
    print("brand mark written")


if __name__ == "__main__":
    main()
