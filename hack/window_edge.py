"""Check that an unmaximized window has no light outline around it.

    python3 hack/window_edge.py shot.png [shot.png ...]

Finds the window by its top bar (the longest run of one color in a row of the
upper third), then looks at the four device pixels just outside its top, left
and right edges. A frame drawn by the app's own client decorations shades
into the desktop there; a 1px border in a light color (the regression this
guards) shows up as an edge pixel much brighter than the top bar. Exits 1
when any shot has one.

Needs Pillow (python3-pil).
"""

import sys

try:
    from PIL import Image
except ImportError:  # pragma: no cover - environment dependent
    sys.exit("window_edge.py needs Pillow (python3-pil)")

# How much brighter than the top bar an edge pixel may be (shadow, antialiasing).
TOLERANCE = 30


def luminance(pixel):
    return 0.2126 * pixel[0] + 0.7152 * pixel[1] + 0.0722 * pixel[2]


def same(a, b):
    return max(abs(x - y) for x, y in zip(a, b)) <= 2


def check(path):
    image = Image.open(path).convert("RGB")
    width, height = image.size
    best = (0, 0, 0, 0)
    for y in range(height // 12, height // 3, 4):
        row = [image.getpixel((x, y)) for x in range(width)]
        start = 0
        for x in range(1, width + 1):
            if x == width or not same(row[x], row[start]):
                if x - start > best[0]:
                    best = (x - start, y, start, x - 1)
                start = x
    _, y, left, right = best
    centre = (left + right) // 2
    bar = image.getpixel((centre, y))
    top = y
    while top > 0 and same(image.getpixel((centre, top - 1)), bar):
        top -= 1
    edge = [image.getpixel((centre, row)) for row in range(max(0, top - 4), top)]
    edge += [image.getpixel((column, y)) for column in range(max(0, left - 4), left)]
    edge += [image.getpixel((column, y)) for column in range(right + 1, min(width, right + 5))]
    brightest = max(luminance(pixel) for pixel in edge)
    ok = brightest <= luminance(bar) + TOLERANCE
    print(
        f"{path}: window {left}..{right} from y {top}, top bar luminance {luminance(bar):.0f}, "
        f"brightest edge pixel {brightest:.0f}: {'ok' if ok else 'LIGHT OUTLINE'}"
    )
    return ok


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    results = [check(path) for path in sys.argv[1:]]
    sys.exit(0 if all(results) else 1)


if __name__ == "__main__":
    main()
