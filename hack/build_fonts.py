"""Rebuild assets/fonts from the font packages the Muxus client bundles.

    uv run --with 'fonttools[woff]' python hack/build_fonts.py ../muxus

The client ships Fontsource's per-script subsets as WOFF2 (`@fontsource-variable/inter`,
`@fontsource/jetbrains-mono`) and `@azurity/pure-nerd-font`. GPUI's text system
reads TrueType/OpenType only, and it rasterizes variable fonts at their default
instance, so the 450 and 550 Inter weights the chrome uses have to exist as
static faces. This script:

- instances every Inter subset at each weight the client uses and merges the
  subsets back into one TTF per weight, named family "Inter" with the exact
  usWeightClass, so a request for weight 450 matches a real 450 face;
- merges the JetBrains Mono subsets per weight (400, 500, 700, and the
  400/700 italics the terminal draws SGR italic with);
- converts the Nerd Font symbols to TTF;
- copies the licenses next to them.
"""

import shutil
import sys
import tempfile
from pathlib import Path

from fontTools.merge import Merger
from fontTools.ttLib import TTFont
from fontTools.varLib.instancer import instantiateVariableFont

INTER_WEIGHTS = [400, 450, 500, 550, 600, 650, 700, 800]
MONO_WEIGHTS = [400, 500, 700]
# Italic faces the terminal renders SGR 3 with (regular and bold).
MONO_ITALIC_WEIGHTS = [400, 700]
SUBSETS = ["latin", "latin-ext", "cyrillic", "cyrillic-ext", "greek", "greek-ext", "vietnamese"]


def pnpm_package(muxus: Path, name: str) -> Path:
    scope, _, pkg = name.partition("/")
    pattern = f"{scope}+{pkg}@*" if pkg else f"{name}@*"
    for candidate in sorted((muxus / "node_modules" / ".pnpm").glob(pattern)):
        path = candidate / "node_modules" / name
        if path.exists():
            return path
    raise SystemExit(f"{name} is not installed in {muxus}; run pnpm install there first")


def woff2_to_ttf(src: Path, dst: Path) -> Path:
    font = TTFont(src)
    font.flavor = None
    font.save(dst)
    return dst


def set_names(font: TTFont, family: str, weight: int, italic: bool = False) -> None:
    style = "Italic" if italic else "Regular"
    suffix = f"{weight}{' Italic' if italic else ''}"
    full = f"{family} {suffix}" if weight != 400 or italic else f"{family} Regular"
    postscript = f"{family.replace(' ', '')}-{weight}{'Italic' if italic else ''}"
    name = font["name"]
    # Drop the variable font's instance and axis names; keep copyright/license.
    keep = {0, 7, 8, 9, 11, 12, 13, 14}
    name.names = [record for record in name.names if record.nameID in keep]
    for name_id, value in {
        1: family,
        2: style,
        3: f"{postscript};muxus-static",
        4: full,
        5: "Version 4.1",
        6: postscript,
        16: family,
        17: suffix if weight != 400 or italic else "Regular",
    }.items():
        name.setName(value, name_id, 3, 1, 0x409)
        name.setName(value, name_id, 1, 0, 0)
    font["OS/2"].usWeightClass = weight
    # Regular/bold bits: only 400 is "regular", nothing here is bold-styled.
    fs = font["OS/2"].fsSelection
    fs &= ~(1 << 5)  # BOLD
    fs = fs | (1 << 0) if italic else fs & ~(1 << 0)  # ITALIC
    fs = fs | (1 << 6) if weight == 400 and not italic else fs & ~(1 << 6)
    font["OS/2"].fsSelection = fs
    font["head"].macStyle = 2 if italic else 0


def merge(paths, dst: Path) -> None:
    merged = Merger().merge([str(p) for p in paths])
    merged.save(dst)


def main() -> None:
    muxus = Path(sys.argv[1] if len(sys.argv) > 1 else "../muxus").resolve()
    out = Path(__file__).resolve().parent.parent / "assets" / "fonts"
    out.mkdir(parents=True, exist_ok=True)
    inter_pkg = pnpm_package(muxus, "@fontsource-variable/inter")
    mono_pkg = pnpm_package(muxus, "@fontsource/jetbrains-mono")
    nerd_pkg = pnpm_package(muxus, "@azurity/pure-nerd-font")

    with tempfile.TemporaryDirectory() as tmp_name:
        tmp = Path(tmp_name)
        variable = {
            subset: woff2_to_ttf(
                inter_pkg / "files" / f"inter-{subset}-wght-normal.woff2", tmp / f"inter-{subset}.ttf"
            )
            for subset in SUBSETS
        }
        for weight in INTER_WEIGHTS:
            parts = []
            for subset, path in variable.items():
                instance = instantiateVariableFont(TTFont(path), {"wght": weight}, updateFontNames=False)
                part = tmp / f"inter-{subset}-{weight}.ttf"
                instance.save(part)
                parts.append(part)
            merged = tmp / f"inter-{weight}.ttf"
            merge(parts, merged)
            font = TTFont(merged)
            set_names(font, "Inter", weight)
            font.save(out / f"Inter-{weight}.ttf")
            print(f"Inter-{weight}.ttf")

        for weight in MONO_WEIGHTS:
            parts = [
                woff2_to_ttf(
                    mono_pkg / "files" / f"jetbrains-mono-{subset}-{weight}-normal.woff2",
                    tmp / f"mono-{subset}-{weight}.ttf",
                )
                for subset in SUBSETS
                if (mono_pkg / "files" / f"jetbrains-mono-{subset}-{weight}-normal.woff2").exists()
            ]
            merged = tmp / f"mono-{weight}.ttf"
            merge(parts, merged)
            font = TTFont(merged)
            set_names(font, "JetBrains Mono", weight)
            font.save(out / f"JetBrainsMono-{weight}.ttf")
            print(f"JetBrainsMono-{weight}.ttf")

        for weight in MONO_ITALIC_WEIGHTS:
            parts = [
                woff2_to_ttf(
                    mono_pkg / "files" / f"jetbrains-mono-{subset}-{weight}-italic.woff2",
                    tmp / f"mono-{subset}-{weight}-italic.ttf",
                )
                for subset in SUBSETS
                if (mono_pkg / "files" / f"jetbrains-mono-{subset}-{weight}-italic.woff2").exists()
            ]
            merged = tmp / f"mono-{weight}-italic.ttf"
            merge(parts, merged)
            font = TTFont(merged)
            set_names(font, "JetBrains Mono", weight, italic=True)
            font.save(out / f"JetBrainsMono-{weight}-Italic.ttf")
            print(f"JetBrainsMono-{weight}-Italic.ttf")

        woff2_to_ttf(nerd_pkg / "PureNerdFont.woff2", out / "PureNerdFont.ttf")
        print("PureNerdFont.ttf")

    shutil.copy(inter_pkg / "LICENSE", out / "Inter-LICENSE.txt")
    shutil.copy(mono_pkg / "LICENSE", out / "JetBrainsMono-LICENSE.txt")
    (out / "PureNerdFont-LICENSE.txt").write_text(
        "PureNerdFont (@azurity/pure-nerd-font 3.x, https://github.com/azurity/pure-nerd-font)\n"
        "is distributed under the MIT license. It repackages the icon glyphs of Nerd Fonts\n"
        "(https://github.com/ryanoasis/nerd-fonts, MIT); the individual icon sets keep the\n"
        "licenses listed at https://github.com/ryanoasis/nerd-fonts/blob/master/license-audit.md\n"
    )


if __name__ == "__main__":
    main()
