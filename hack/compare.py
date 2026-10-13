"""Compare two screenshots of the same size.

    python3 hack/compare.py ours.png reference.png out-prefix [--crop x,y,w,h] [--scale 2]

Writes:
  <out-prefix>-side.png   ours | reference, side by side
  <out-prefix>-diff.png   per-pixel difference, amplified, over a dimmed copy of ours
  <out-prefix>-blink.gif  the two alternating, which shows sub-pixel shifts best
and prints the share of pixels that differ and the bounding box of the
differences. `--crop` takes CSS pixels (as the docs screenshots are 2x, pass
`--scale 2`, the default, to have them multiplied).

Needs Pillow (python3-pil); falls back to an error message without it.
"""

import argparse
import sys

try:
    from PIL import Image, ImageChops, ImageEnhance
except ImportError:  # pragma: no cover - environment dependent
    sys.exit("compare.py needs Pillow (python3-pil); try: uv run --with pillow python hack/compare.py ...")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("ours")
    parser.add_argument("reference")
    parser.add_argument("out")
    parser.add_argument("--crop", help="x,y,w,h in CSS pixels")
    parser.add_argument("--scale", type=float, default=2.0)
    parser.add_argument("--threshold", type=int, default=24, help="per-channel difference that counts")
    args = parser.parse_args()

    ours = Image.open(args.ours).convert("RGB")
    ref = Image.open(args.reference).convert("RGB")
    if ours.size != ref.size:
        print(f"size differs: ours {ours.size}, reference {ref.size}; comparing the overlap")
        w, h = min(ours.width, ref.width), min(ours.height, ref.height)
        ours, ref = ours.crop((0, 0, w, h)), ref.crop((0, 0, w, h))
    if args.crop:
        x, y, w, h = (float(v) * args.scale for v in args.crop.split(","))
        box = (int(x), int(y), int(x + w), int(y + h))
        ours, ref = ours.crop(box), ref.crop(box)

    side = Image.new("RGB", (ours.width * 2 + 8, ours.height), (255, 0, 255))
    side.paste(ours, (0, 0))
    side.paste(ref, (ours.width + 8, 0))
    side.save(f"{args.out}-side.png")

    diff = ImageChops.difference(ours, ref)
    mask = diff.convert("L").point(lambda v: 255 if v > args.threshold else 0)
    base = ImageEnhance.Brightness(ours.convert("L").convert("RGB")).enhance(0.35)
    red = Image.new("RGB", ours.size, (255, 40, 40))
    overlay = Image.composite(red, base, mask)
    overlay.save(f"{args.out}-diff.png")

    ours.save(f"{args.out}-blink.gif", save_all=True, append_images=[ref], duration=600, loop=0)

    hist = mask.histogram()
    changed = hist[255]
    total = ours.width * ours.height
    print(f"differing pixels: {changed} of {total} ({100.0 * changed / total:.2f}%)")
    print(f"difference bbox (device px): {mask.getbbox()}")


if __name__ == "__main__":
    main()
