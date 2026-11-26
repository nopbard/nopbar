"""Make website WebPs and a contact sheet from the offscreen PNGs."""

import argparse
from pathlib import Path

from PIL import Image, ImageDraw


SCENES = ("desktop", "topbar-island", "quick-settings", "widgets", "dock", "gnome")
LOOKS = ("light", "dark")
MAX_WEBP_BYTES = 420_000


def webp(image: Image.Image, path: Path) -> None:
    # Preserve the native 2x dimensions for the site.
    for quality in (85, 83, 81, 79):
        image.save(path, "WEBP", quality=quality, method=6)
        if path.stat().st_size <= MAX_WEBP_BYTES:
            break


def pack(root: Path) -> None:
    gap, width, max_height, caption = 18, 612, 350, 20
    rows = []
    for scene in SCENES:
        row = []
        for look in LOOKS:
            stem = f"{scene}-{look}"
            with Image.open(root / f"{stem}.png") as source:
                image = source.convert("RGB")
            webp(image.copy(), root / f"{stem}.webp")
            image.thumbnail((width, max_height), Image.Resampling.LANCZOS)
            row.append((stem, image))
        rows.append(row)
    sheet = Image.new(
        "RGB",
        (width * len(LOOKS) + gap * (len(LOOKS) + 1),
         sum(max(image.height for _, image in row) + caption + gap for row in rows) + gap),
        "#171717",
    )
    labels = ImageDraw.Draw(sheet)
    y = gap
    for row in rows:
        for column, (stem, image) in enumerate(row):
            x = gap + column * (width + gap)
            labels.text((x, y), stem, fill="#d9d9d9")
            sheet.paste(image, (x + (width - image.width) // 2, y + caption))
        y += max(image.height for _, image in row) + caption + gap
    sheet.save(root / "all.png")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    pack(parser.parse_args().directory)
