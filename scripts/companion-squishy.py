"""Package genuine WARP before/after frames. No screen capture or app input."""
from pathlib import Path
import argparse
import json
from concurrent.futures import ProcessPoolExecutor
from PIL import Image, ImageDraw


def package_scene(scene: Path):
    before, after = scene / "before", scene / "after"
    a, b = sorted(before.glob("*.png")), sorted(after.glob("*.png"))
    assert len(a) == len(b) == 120, scene
    frames = []
    for index, (old, new) in enumerate(zip(a, b)):
        with Image.open(old) as im_a, Image.open(new) as im_b:
            assert im_a.size == im_b.size
            w, h = im_a.size
            im = Image.new("RGB", (w * 2, h + 30), (48, 48, 48))
            im.paste(im_a, (0, 30))
            im.paste(im_b, (w, 30))
            draw = ImageDraw.Draw(im)
            draw.text((10, 8), f"BEFORE 0.1.85   {index / 30:.2f}s", fill="white")
            draw.text((w + 10, 8), "AFTER", fill="white")
            frames.append(im)
    # 33/34ms cadence totals exactly four seconds, nominal 30fps.
    durations = [round((i + 1) * 1000 / 30) - round(i * 1000 / 30) for i in range(120)]
    movie = scene / "comparison.webp"
    frames[0].save(movie, save_all=True, append_images=frames[1:],
                   duration=durations, loop=0, quality=95, method=1)
    indices = [0, 11, 14, 18, 23, 30, 33, 36, 39, 42, 48, 60, 72, 90, 108, 119]
    thumb_width = 360
    thumb_height = round(frames[0].height * thumb_width / frames[0].width)
    timeline = Image.new("RGB", (thumb_width * 4, (thumb_height + 24) * 4), (48, 48, 48))
    for cell, index in enumerate(indices):
        x, y = (cell % 4) * thumb_width, (cell // 4) * (thumb_height + 24)
        thumb = frames[index].resize((thumb_width, thumb_height), Image.Resampling.LANCZOS)
        timeline.paste(thumb, (x, y + 24))
        ImageDraw.Draw(timeline).text((x + 5, y + 5), f"{index / 30:.2f}s", fill="white")
    timeline.save(scene / "timeline.png")
    with Image.open(movie) as check:
        assert check.is_animated and check.n_frames == 120
        total = 0
        for i in range(check.n_frames):
            check.seek(i)
            check.load()
            total += check.info["duration"]
        assert total == 4000
    result = {"scenario": scene.name, "frames": 120, "fps": 30,
                     "duration_ms": 4000, "webp": str(movie),
                     "timeline": str(scene / "timeline.png")}
    print(scene.name, flush=True)
    return result


def package(root: Path, pattern: str = "*"):
    scenes = [s for s in sorted(root.glob(pattern)) if (s / "before").is_dir() and (s / "after").is_dir()]
    with ProcessPoolExecutor(max_workers=2) as pool:
        manifest = list(pool.map(package_scene, scenes))
    (root / "manifest.json").write_text(json.dumps(manifest, indent=2), encoding="utf-8")
    cards = "\n".join(f'<section><h2>{r["scenario"]}</h2><img loading="lazy" src="{r["scenario"]}/comparison.webp"><br><img loading="lazy" src="{r["scenario"]}/timeline.png"></section>' for r in manifest)
    (root / "index.html").write_text('<!doctype html><meta charset="utf-8"><title>Companion squishy all</title><style>body{background:#202020;color:#ddd;font:14px sans-serif}img{max-width:100%}section{margin-bottom:48px}</style>' + cards, encoding="utf-8")
    print(f"{len(manifest)} comparisons", flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("root", type=Path, nargs="?", default=Path("target/squishy-all"))
    parser.add_argument("--filter", default="*")
    args = parser.parse_args()
    package(args.root.resolve(), args.filter)
