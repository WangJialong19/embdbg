#!/usr/bin/env python3
"""Builds the fonts that embdbg embeds.

Inputs are downloaded separately (see README.md next to this script):
  <dl>/notosans/NotoSansSC-VF.ttf   Noto Sans SC, variable weight (SIL OFL 1.1)
  <dl>/notosans/OFL.txt
  <dl>/tabler-extract/package/      @tabler/icons-webfont (MIT)

Outputs:
  <out>/NotoSansSC-Regular.subset.ttf   CJK text font, cut to the characters a Chinese UI needs
  <out>/TablerIcons-Outline.subset.ttf  icon fonts, cut to the icons listed in --icons
  <out>/TablerIcons-Filled.subset.ttf
  <out>/LICENSE-*.txt
  <rust>                               generated Rust module with the icon codepoints

Run from the project root, with an isolated interpreter:

  python -I tools/fonts/build_fonts.py --dl .tmp-dl --out assets/fonts \\
      --rust crates/embdbg-app/src/icons.rs --icons tools/fonts/icons.txt
"""

import argparse
import re
import sys
from pathlib import Path

from fontTools import subset
from fontTools.ttLib import TTFont
from fontTools.varLib import instancer

# Everything outside the CJK ideographs that a technical UI in Chinese tends to show.
NOTO_UNICODE_RANGES = [
    (0x0020, 0x007E),  # ASCII
    (0x00A0, 0x00FF),  # Latin-1: degree, micro, plus-minus, multiplication, division
    (0x0370, 0x03FF),  # Greek: ohm, pi, delta
    (0x2000, 0x206F),  # dashes, quotes, ellipsis, per mille
    (0x20AC, 0x20AC),  # euro sign
    (0x2122, 0x2122),  # trade mark
    (0x2190, 0x21FF),  # arrows
    (0x2200, 0x22FF),  # math: minus, approx, compare, infinity, sum, root
    (0x25A0, 0x25FF),  # geometric shapes
    (0x3000, 0x303F),  # CJK punctuation
    (0xFF00, 0xFFEF),  # fullwidth forms
]


def gb2312_hanzi() -> str:
    """The 6763 hanzi of GB2312, which cover virtually all simplified Chinese UI text."""
    chars = []
    for lead in range(0xB0, 0xF8):
        for trail in range(0xA1, 0xFF):
            try:
                chars.append(bytes([lead, trail]).decode("gb2312"))
            except UnicodeDecodeError:
                pass
    return "".join(chars)


def cut(font: TTFont, *, unicodes=(), text="", hinting=True, features=None) -> TTFont:
    options = subset.Options()
    options.hinting = hinting
    options.notdef_outline = True
    if features is not None:
        options.layout_features = features
    subsetter = subset.Subsetter(options)
    subsetter.populate(unicodes=unicodes, text=text)
    subsetter.subset(font)
    return font


def unicodes_of(ranges) -> list[int]:
    return [cp for start, end in ranges for cp in range(start, end + 1)]


def build_noto(dl: Path, out: Path) -> None:
    font = TTFont(dl / "notosans" / "NotoSansSC-VF.ttf")
    # Pin the variable font to its regular weight: the text backend renders one instance anyway.
    font = instancer.instantiateVariableFont(font, {"wght": 400}, inplace=False, updateFontNames=True)
    cut(font, unicodes=unicodes_of(NOTO_UNICODE_RANGES), text=gb2312_hanzi())
    target = out / "NotoSansSC-Regular.subset.ttf"
    font.save(target)
    print(f"{target.name}: {len(font.getGlyphOrder())} glyphs, {target.stat().st_size / 1e6:.2f} MB")
    (out / "LICENSE-NotoSansSC.txt").write_bytes((dl / "notosans" / "OFL.txt").read_bytes())


CSS_RULE = re.compile(r'\.ti-([a-z0-9-]+):before\s*\{\s*content:\s*"\\([0-9a-f]+)"\s*;')


def read_codepoints(css: Path) -> dict[str, int]:
    return {name: int(code, 16) for name, code in CSS_RULE.findall(css.read_text(encoding="utf-8"))}


def read_icon_list(path: Path) -> list[str]:
    names = []
    for line in path.read_text(encoding="utf-8").splitlines():
        name = line.split("#", 1)[0].strip()
        if name:
            names.append(name)
    return names


def build_tabler(dl: Path, out: Path, names: list[str]) -> dict[str, dict[str, int]]:
    package = dl / "tabler-extract" / "package"
    variants = {
        "outline": ("tabler-icons", "TablerIcons-Outline.subset.ttf"),
        "filled": ("tabler-icons-filled", "TablerIcons-Filled.subset.ttf"),
    }
    kept: dict[str, dict[str, int]] = {}
    for variant, (source, target_name) in variants.items():
        table = read_codepoints(package / "dist" / f"{source}.css")
        missing = [name for name in names if name not in table]
        if variant == "outline" and missing:
            sys.exit(f"icons.txt names unknown to Tabler's outline set: {', '.join(missing)}")
        codes = {name: table[name] for name in names if name in table}
        kept[variant] = codes
        font = TTFont(package / "dist" / "fonts" / f"{source}.ttf")
        # Icons carry no text shaping, and hints add nothing at the sizes they are drawn at.
        cut(font, unicodes=codes.values(), hinting=False, features=[])
        target = out / target_name
        font.save(target)
        print(f"{target.name}: {len(codes)} icons, {target.stat().st_size / 1e3:.0f} kB"
              + (f" ({len(missing)} not available filled)" if missing else ""))
    (out / "LICENSE-TablerIcons.txt").write_bytes((package / "LICENSE").read_bytes())
    return kept


def write_rust(path: Path, kept: dict[str, dict[str, int]]) -> None:
    lines = [
        "//! Icon codepoints of the embedded Tabler Icons subsets (https://tabler.io/icons, MIT).",
        "//!",
        "//! Generated by `tools/fonts/build_fonts.py`; do not edit by hand.",
        "",
    ]
    for variant, codes in kept.items():
        lines += [
            f"/// Icons of the {variant} font, [`crate::fonts::ICON_{variant.upper()}_FONT`].",
            "#[allow(dead_code)] // a lookup table: only the icons a view uses are referenced",
            f"pub mod {variant} {{",
        ]
        for name, code in sorted(codes.items()):
            const = name.upper().replace("-", "_")
            lines.append(f'    pub const {const}: &str = "\\u{{{code:x}}}";')
        lines += ["}", ""]
    path.write_text("\n".join(lines), encoding="utf-8", newline="\n")
    print(f"{path}: generated")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--dl", type=Path, required=True, help="directory holding the downloaded inputs")
    parser.add_argument("--out", type=Path, required=True, help="assets directory for the font files")
    parser.add_argument("--rust", type=Path, required=True, help="Rust module to generate")
    parser.add_argument("--icons", type=Path, required=True, help="list of Tabler icon names to embed")
    args = parser.parse_args()

    args.out.mkdir(parents=True, exist_ok=True)
    args.rust.parent.mkdir(parents=True, exist_ok=True)
    build_noto(args.dl, args.out)
    kept = build_tabler(args.dl, args.out, read_icon_list(args.icons))
    write_rust(args.rust, kept)


if __name__ == "__main__":
    main()
