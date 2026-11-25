"""Scores NopBar's icon cache: source frames that are not real ones, and
pixels stored with the wrong alpha mode. Read-only; no screenshots.

Reads the dock's disk cache:
    %LOCALAPPDATA%\\NopBar\\icons\\*.bgra
File format (dock/src/icons.rs, "disk cache"): u32 width, u32 height
(little endian), then width*height*4 bytes of BGRA, top-down. Names:
    {fnv64(key + mtime)}-{size}.bgra     older builds (straight alpha as the
                                         shell gave it, asked at the peak size)
    {fnv64(key + mtime)}-{tag}-pm.bgra   current builds (premultiplied;
                                         tag "src256" for the dock)

Per icon, on its pixels:

  straight - share of pixels with a color channel above their alpha. A
             premultiplied picture has none; the dock draws every icon as
             premultiplied, so any such pixel is drawn too bright (its
             antialiased edge becomes a hard, bright stair-stepped rim).
  boxed    - the shell's "no frame this large" answer: a small frame in
             the middle of the canvas with a faint frame around the edge
             (the ring one pixel in is visible, the clearly visible part at
             most 60 % of the side). Same rule as icons.rs `boxed`.
  block    - share of partly transparent pixels equal to their right, lower
             and lower-right neighbors (2x2 blocks of a nearest-neighbor
             stretch; about 0 for real art). icons.rs `block_share`.
  width    - average alpha transition width (partly transparent pixels per
             crossing of the 50 % line). Informational only: soft art
             (shadows, glass) is wide too, so it is not used to judge.

Verdict per icon (icons.rs `looks_upscaled` plus the alpha check):
  ok / STRAIGHT (drawn with bright rims) / BOXED / BLOCKY.

Usage:
    python scripts/icon_cache_score.py [cache dir] [--csv out.csv]
Plain Python 3, no packages needed.
"""

import os
import struct
import sys

BLOCK_UPSCALED = 0.12   # icons.rs blocky()
LO, HI = 16, 240        # "partly transparent": LO < a < HI (17..=239)


def read_bgra(path):
    with open(path, "rb") as f:
        data = f.read()
    if len(data) < 8:
        return None
    w, h = struct.unpack("<II", data[:8])
    if w == 0 or h == 0 or w > 4096 or h > 4096 or len(data) != 8 + w * h * 4:
        return None
    return w, h, data[8:]


def boxed(w, h, px):
    if w < 64 or h < 64:
        return False
    a = px[3::4]
    ring = [a[1 * w + x] for x in range(1, w - 1)] + [a[(h - 2) * w + x] for x in range(1, w - 1)]
    ring += [a[y * w + 1] for y in range(1, h - 1)] + [a[y * w + w - 2] for y in range(1, h - 1)]
    if sum(1 for v in ring if v > 0) * 10 < len(ring) * 9:
        return False
    xs = [i % w for i in range(w * h) if a[i] > 96]
    if not xs:
        return False
    ys = [i // w for i in range(w * h) if a[i] > 96]
    side = max(max(xs) - min(xs), max(ys) - min(ys)) + 1
    return side * 10 <= min(w, h) * 6


def score(w, h, px):
    """dict of the measures above for BGRA bytes."""
    a = px[3::4]
    partial = cross = blocks = straight = 0
    for y in range(h):
        row = y * w
        for x in range(w):
            i = row + x
            v = a[i]
            o = i * 4
            if max(px[o], px[o + 1], px[o + 2]) > v:
                straight += 1
            p = LO < v < HI
            if p:
                partial += 1
            if x + 1 < w and (v >= 128) != (a[i + 1] >= 128):
                cross += 1
            if y + 1 < h and (v >= 128) != (a[i + w] >= 128):
                cross += 1
            if p and x + 1 < w and y + 1 < h:
                q = px[o:o + 4]
                if q == px[o + 4:o + 8] and q == px[o + w * 4:o + w * 4 + 4] and q == px[o + w * 4 + 4:o + w * 4 + 8]:
                    blocks += 1
    return {
        "straight": straight / (w * h),
        "block": blocks / partial if partial >= 64 else 0.0,
        "width": partial / cross if cross else 0.0,
        "boxed": boxed(w, h, px),
    }


def verdict(m):
    if m["boxed"]:
        return "BOXED"
    if m["block"] >= BLOCK_UPSCALED:
        return "BLOCKY"
    if m["straight"] > 0:
        return "STRAIGHT"
    return "ok"


def main(argv):
    csv = None
    args = []
    it = iter(argv)
    for a in it:
        if a == "--csv":
            csv = next(it)
        else:
            args.append(a)
    d = args[0] if args else os.path.join(os.environ.get("LOCALAPPDATA", ""), "NopBar", "icons")
    rows = []
    for name in sorted(os.listdir(d)):
        if not name.endswith(".bgra"):
            continue
        r = read_bgra(os.path.join(d, name))
        if r is None:
            rows.append((name, 0, None, "unreadable"))
            continue
        w, h, px = r
        m = score(w, h, px)
        rows.append((name, w, m, verdict(m)))
    print(f"{'file':34} {'side':>5} {'straight':>8} {'block':>6} {'width':>6} {'boxed':>5}  verdict")
    for name, w, m, v in rows:
        if m is None:
            print(f"{name:34} {'':5} {'':8} {'':6} {'':6} {'':5}  {v}")
            continue
        print(f"{name:34} {w:5} {m['straight']:8.3f} {m['block']:6.3f} {m['width']:6.2f} {str(m['boxed'])[0]:>5}  {v}")
    scored = [r for r in rows if r[2] is not None]
    print()
    print(f"files: {len(rows)}  readable: {len(scored)}")
    for v in ("ok", "STRAIGHT", "BOXED", "BLOCKY"):
        n = sum(1 for r in scored if r[3] == v)
        print(f"  {v:9} {n:4} ({100.0 * n / max(1, len(scored)):3.0f} %)")
    n_str = sum(1 for r in scored if r[2]["straight"] > 0)
    print(f"  any pixel above its alpha (wrong alpha mode, any verdict): {n_str} ({100.0 * n_str / max(1, len(scored)):.0f} %)")
    if scored:
        bs = sorted(r[2]["block"] for r in scored)
        print(f"  block share: median {bs[len(bs) // 2]:.3f}, max {bs[-1]:.3f}")
    # By what was asked of the shell (the name's suffix).
    by = {}
    for r in scored:
        by.setdefault(r[0].split("-", 1)[1] if "-" in r[0] else "?", []).append(r)
    print("  by name suffix (what was asked):")
    for k in sorted(by, key=lambda s: (len(s), s)):
        g = by[k]
        cnt = lambda v: sum(1 for r in g if r[3] == v)
        print(f"    {k:16} n={len(g):4}  ok={cnt('ok'):4}  straight={cnt('STRAIGHT'):4}  boxed={cnt('BOXED'):3}  blocky={cnt('BLOCKY'):3}")
    if csv:
        with open(csv, "w", encoding="utf-8") as f:
            f.write("file,side,straight,block,width,boxed,verdict\n")
            for name, w, m, v in rows:
                if m is None:
                    f.write(f"{name},,,,,,{v}\n")
                else:
                    f.write(f"{name},{w},{m['straight']:.4f},{m['block']:.4f},{m['width']:.3f},{int(m['boxed'])},{v}\n")


if __name__ == "__main__":
    main(sys.argv[1:])
