#!/usr/bin/env python3
"""Generate assets/haunt-banner.svg, the header image for the README.

Pixel art on one grid: every block is a multiple of 4px and every figure is a
run of whole cells, so the banner is crisp at any width and there is nothing to
anti-alias. Flat colour only, from the same handful of darks, greys and the one
cyan the terminal itself uses: no gradients, no filters, no animation.

The ghost is the one the binary prints, not a redrawn copy of it. It is taken
from tools/gen-ghost.py, which is what assets/ghost.png is made of, so the two
cannot drift apart, and it is drawn with nothing added: its own dark eyes are
the expression. Change the cell size or the placement with the constants below
and regenerate:

    python3 tools/gen-banner.py           # write assets/haunt-banner.svg
    python3 tools/gen-banner.py --cells   # dump the sprite's cells, to read them

The write is refused if the parts stop lining up: see check().
"""

import importlib.util
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
# 264 rather than 260: the content band, 8px bars in and out, is then even in
# height, so the window and the text block can be centred on it without leaving
# the grid. The centred line is (8 + 256) / 2.
W, H = 960, 264
CENTRE = (8 + H - 8) // 2

# The banner's palette: the page and pane darks, the frame grey, one cyan for
# emphasis, and a violet for the stray pixels. The ghost brings its own outline
# from gen-ghost.py, which is black, so the pane has to sit above black for the
# border to read: #0d0e0f is too close to it, and a real theme background is
# nowhere near that dark.
BG = "#16181a"
PANEL = "#191d23"
BAR = "#3c4048"
FRAME = "#5c6370"
CYAN = "#5ef1ff"
VIOLET = "#bd5eff"
WHITE = "#ffffff"

# Wordmark: block letters on an 8px grid, so they read as pixels and not as a
# font. Five glyphs, 52 apart, 248 wide overall.
WORDMARK = (
    ("M0 0h8v56H0zM32 0h8v56h-8zM8 24h24v8H8z", 0),                    # H
    ("M8 0h24v8H8zM0 8h8v48H0zM32 8h8v48h-8zM8 24h24v8H8z", 52),        # A
    ("M0 0h8v48H0zM32 0h8v48h-8zM8 48h24v8H8z", 104),                  # U
    ("M0 0h8v56H0zM32 0h8v56h-8zM8 8h8v8H8zM16 16h8v16h-8zM24 32h8v8h-8z", 156),  # N
    ("M0 0h40v8H0zM16 8h8v48h-8z", 208),                                # T
)
WORDMARK_AT = (58, 64)
RULE_AT = (58, 128, 248)   # the cyan line under the wordmark: x, y, width

# The slogan is set in a 5x7 bitmap font, one <path> per letter, so the text is
# drawn rather than left to whatever font the viewer happens to have. CELL is a
# font pixel in device pixels: 3 keeps both lines inside the left column.
SLOGAN_CELL = 3
WORD_GAP = 3              # font pixels between words: one column, then a blank
GLYPHS = {
    "a": "M1 1h1v1h-1zM2 1h1v1h-1zM3 1h1v1h-1zM4 2h1v1h-1zM1 3h1v1h-1zM2 3h1v1h-1zM3 3h1v1h-1zM4 3h1v1h-1zM0 4h1v1h-1zM4 4h1v1h-1zM0 5h1v1h-1zM4 5h1v1h-1zM1 6h1v1h-1zM2 6h1v1h-1zM3 6h1v1h-1zM4 6h1v1h-1z",
    "c": "M1 1h1v1h-1zM2 1h1v1h-1zM3 1h1v1h-1zM4 1h1v1h-1zM0 2h1v1h-1zM0 3h1v1h-1zM0 4h1v1h-1zM0 5h1v1h-1zM1 6h1v1h-1zM2 6h1v1h-1zM3 6h1v1h-1zM4 6h1v1h-1z",
    "d": "M4 0h1v1h-1zM4 1h1v1h-1zM1 2h1v1h-1zM2 2h1v1h-1zM3 2h1v1h-1zM4 2h1v1h-1zM0 3h1v1h-1zM4 3h1v1h-1zM0 4h1v1h-1zM4 4h1v1h-1zM0 5h1v1h-1zM4 5h1v1h-1zM1 6h1v1h-1zM2 6h1v1h-1zM3 6h1v1h-1zM4 6h1v1h-1z",
    "e": "M1 1h1v1h-1zM2 1h1v1h-1zM3 1h1v1h-1zM0 2h1v1h-1zM4 2h1v1h-1zM0 3h1v1h-1zM1 3h1v1h-1zM2 3h1v1h-1zM3 3h1v1h-1zM4 3h1v1h-1zM0 4h1v1h-1zM0 5h1v1h-1zM1 6h1v1h-1zM2 6h1v1h-1zM3 6h1v1h-1zM4 6h1v1h-1z",
    "h": "M0 0h1v1h-1zM0 1h1v1h-1zM0 2h1v1h-1zM2 2h1v1h-1zM3 2h1v1h-1zM0 3h1v1h-1zM1 3h1v1h-1zM4 3h1v1h-1zM0 4h1v1h-1zM4 4h1v1h-1zM0 5h1v1h-1zM4 5h1v1h-1zM0 6h1v1h-1zM4 6h1v1h-1z",
    "i": "M2 0h1v1h-1zM1 2h1v1h-1zM2 2h1v1h-1zM2 3h1v1h-1zM2 4h1v1h-1zM2 5h1v1h-1zM1 6h1v1h-1zM2 6h1v1h-1zM3 6h1v1h-1z",
    "l": "M1 0h1v1h-1zM2 0h1v1h-1zM2 1h1v1h-1zM2 2h1v1h-1zM2 3h1v1h-1zM2 4h1v1h-1zM2 5h1v1h-1zM1 6h1v1h-1zM2 6h1v1h-1zM3 6h1v1h-1z",
    "m": "M0 1h1v1h-1zM1 1h1v1h-1zM3 1h1v1h-1zM0 2h1v1h-1zM2 2h1v1h-1zM4 2h1v1h-1zM0 3h1v1h-1zM2 3h1v1h-1zM4 3h1v1h-1zM0 4h1v1h-1zM2 4h1v1h-1zM4 4h1v1h-1zM0 5h1v1h-1zM2 5h1v1h-1zM4 5h1v1h-1zM0 6h1v1h-1zM2 6h1v1h-1zM4 6h1v1h-1z",
    "n": "M0 1h1v1h-1zM2 1h1v1h-1zM3 1h1v1h-1zM0 2h1v1h-1zM1 2h1v1h-1zM4 2h1v1h-1zM0 3h1v1h-1zM4 3h1v1h-1zM0 4h1v1h-1zM4 4h1v1h-1zM0 5h1v1h-1zM4 5h1v1h-1zM0 6h1v1h-1zM4 6h1v1h-1z",
    "o": "M1 1h1v1h-1zM2 1h1v1h-1zM3 1h1v1h-1zM0 2h1v1h-1zM4 2h1v1h-1zM0 3h1v1h-1zM4 3h1v1h-1zM0 4h1v1h-1zM4 4h1v1h-1zM0 5h1v1h-1zM4 5h1v1h-1zM1 6h1v1h-1zM2 6h1v1h-1zM3 6h1v1h-1z",
    "p": "M0 1h1v1h-1zM1 1h1v1h-1zM2 1h1v1h-1zM3 1h1v1h-1zM0 2h1v1h-1zM4 2h1v1h-1zM0 3h1v1h-1zM4 3h1v1h-1zM0 4h1v1h-1zM1 4h1v1h-1zM2 4h1v1h-1zM3 4h1v1h-1zM0 5h1v1h-1zM0 6h1v1h-1z",
    "r": "M0 1h1v1h-1zM2 1h1v1h-1zM3 1h1v1h-1zM0 2h1v1h-1zM1 2h1v1h-1zM4 2h1v1h-1zM0 3h1v1h-1zM0 4h1v1h-1zM0 5h1v1h-1zM0 6h1v1h-1z",
    "s": "M1 1h1v1h-1zM2 1h1v1h-1zM3 1h1v1h-1zM4 1h1v1h-1zM0 2h1v1h-1zM1 3h1v1h-1zM2 3h1v1h-1zM3 3h1v1h-1zM4 4h1v1h-1zM4 5h1v1h-1zM0 6h1v1h-1zM1 6h1v1h-1zM2 6h1v1h-1zM3 6h1v1h-1z",
    "t": "M1 0h1v1h-1zM1 1h1v1h-1zM0 2h1v1h-1zM1 2h1v1h-1zM2 2h1v1h-1zM3 2h1v1h-1zM1 3h1v1h-1zM1 4h1v1h-1zM1 5h1v1h-1zM4 5h1v1h-1zM2 6h1v1h-1zM3 6h1v1h-1z",
    "u": "M0 1h1v1h-1zM4 1h1v1h-1zM0 2h1v1h-1zM4 2h1v1h-1zM0 3h1v1h-1zM4 3h1v1h-1zM0 4h1v1h-1zM4 4h1v1h-1zM0 5h1v1h-1zM3 5h1v1h-1zM4 5h1v1h-1zM1 6h1v1h-1zM2 6h1v1h-1zM4 6h1v1h-1z",
    "x": "M0 1h1v1h-1zM4 1h1v1h-1zM1 2h1v1h-1zM3 2h1v1h-1zM2 3h1v1h-1zM2 4h1v1h-1zM1 5h1v1h-1zM3 5h1v1h-1zM0 6h1v1h-1zM4 6h1v1h-1z",
    "y": "M0 1h1v1h-1zM4 1h1v1h-1zM0 2h1v1h-1zM4 2h1v1h-1zM1 3h1v1h-1zM2 3h1v1h-1zM3 3h1v1h-1zM4 3h1v1h-1zM4 4h1v1h-1zM0 5h1v1h-1zM4 5h1v1h-1zM1 6h1v1h-1zM2 6h1v1h-1zM3 6h1v1h-1z",
}
# x, y of each line, in device pixels: one line of leading between them, both
# starting under the wordmark, and the pair ending at the same height as the
# window's foot so the two columns sit on the same middle line.
SLOGAN = (
    ("muxed persistent processes", 58, 148),
    ("that haunt your terminal", 58, 180),
)

# The pane the ghost is haunting. The frame is 8px, one bar of the wordmark's
# grid, and the window is sized around the sprite rather than the other way
# round: the ghost is drawn over it, so its head breaks the top edge. That is
# where it came out of. Height 184, centred on the content band.
WINDOW = (680, 40, 256, 184)
GHOST_CELL = 7
# The sprite is 189 tall and the window 184, so it is placed 12px high: its
# head crosses the top frame into the page, its foot stays inside the pane.
GHOST_AT = (700, 28)


# x, y of the stray pixels, colour first. None of them may fall in the window
# or under the bars, which is what check() holds them to.
DUST = (
    (CYAN, 36, 44), (CYAN, 500, 84), (CYAN, 556, 224),
    (VIOLET, 466, 60), (VIOLET, 148, 228), (VIOLET, 916, 236),
    (FRAME, 944, 120), (FRAME, 652, 72), (FRAME, 948, 208), (FRAME, 196, 20),
)


def load_ghost():
    """The banner ghost, as cells, straight out of the tool that writes the PNG."""
    path = os.path.join(ROOT, "tools", "gen-ghost.py")
    spec = importlib.util.spec_from_file_location("gen_ghost", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    module.check()
    return module


def wordmark():
    x, y = WORDMARK_AT
    lines = [f'  <g transform="translate({x} {y})" fill="{WHITE}">']
    for d, offset in WORDMARK:
        at = f' transform="translate({offset})"' if offset else ""
        lines.append(f'    <path d="{d}"{at}/>')
    lines.append("  </g>")
    return "\n".join(lines)


def slogan():
    defs = "\n".join(
        f'    <path id="glyph-{c}" d="{d}"/>' for c, d in sorted(GLYPHS.items())
    )
    blocks = []
    for text, x, y in SLOGAN:
        uses, column = [], 0
        for char in text:
            if char == " ":
                column += WORD_GAP
                continue
            uses.append(f'    <use href="#glyph-{char}" x="{column}"/>')
            column += 5 + WORD_GAP
        blocks.append(
            f'  <g transform="translate({x} {y}) scale({SLOGAN_CELL})" fill="{WHITE}">\n'
            + "\n".join(uses)
            + "\n  </g>"
        )
    return f"  <defs>\n{defs}\n  </defs>\n" + "\n".join(blocks)


def window():
    x, y, w, h = WINDOW
    return "\n".join(
        (
            f'  <path d="M{x} {y}h{w}v{h}H{x}z" fill="{FRAME}"/>',
            f'  <path d="M{x + 8} {y + 8}h{w - 16}v{h - 16}h-{w - 16}z" fill="{PANEL}"/>',
        )
    )


def pane_text():
    """What the pane printed before the ghost arrived.

    Deliberately little: the sprite owns the window, so the prompt sits in the
    one corner of it the head and the arm leave free, and the widths stop short
    of the sprite's cells.
    """
    x = WINDOW[0] + 20
    top = WINDOW[1] + 28
    rows = [
        (CYAN, f"M{x} {top}h8v8h8v8h-8v8h-8v-8h8v-8h-8z"),  # the prompt's arrow
        (FRAME, f"M{x + 24} {top + 8}h48v8h-48z"),          # and its command
        (BAR, f"M{x} {top + 40}h56v8h-56z"),                # printed output
        (CYAN, f"M{x} {top + 64}h24v8h-24z"),               # a running process
        (FRAME, f"M{x} {top + 84}h16v8h-16z"),              # the cursor it left
    ]
    return "\n".join(f'  <path d="{d}" fill="{c}"/>' for c, d in rows)


def ghost(ghost_module):
    """The sprite, as gen-ghost.py draws it and nothing added to it."""
    gx, gy = GHOST_AT
    return "\n".join(
        "  " + line
        for line in ghost_module.svg(ghost_module.grid(), gx, gy, GHOST_CELL).splitlines()
    )


def dust():
    """A few lone pixels in the dark, the only ornament the banner is allowed."""
    return "\n".join(f'  <path d="M{x} {y}h4v4h-4z" fill="{c}"/>' for c, x, y in DUST)


def build():
    ghost_module = load_ghost()
    rx, ry, rw = RULE_AT
    return "\n".join(
        (
            f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" role="img"'
            ' aria-labelledby="title desc" shape-rendering="crispEdges">',
            '  <title id="title">haunt</title>',
            '  <desc id="desc">Pixel art of a ghost in a terminal beside the word'
            " haunt and the slogan: muxed persistent processes that haunt your"
            " terminal.</desc>",
            f'  <rect width="{W}" height="{H}" fill="{BG}"/>',
            f'  <path d="M0 0h{W}v8H0zM0 {H - 8}h{W}v8H0z" fill="{BAR}"/>',
            dust(),
            wordmark(),
            f'  <path d="M{rx} {ry}h{rw}v8H{rx}z" fill="{CYAN}"/>',
            slogan(),
            window(),
            pane_text(),
            f"  <!-- Ghost, drawn by tools/gen-ghost.py at {GHOST_CELL}px per cell -->",
            ghost(ghost_module),
            "</svg>",
        )
    ) + "\n"


def check(out):
    """Refuse to write a banner whose parts have stopped lining up.

    These are the invariants that are easy to break one at a time: moving the
    window without the text, or the sprite without its cell, or scattering a
    pixel into the art. Each one is worth more as an assertion than as a look
    at a render.
    """
    x, y, w, h = WINDOW
    assert y + h / 2 == CENTRE, f"window midline {y + h / 2}, banner centre {CENTRE}"

    top, bottom = WORDMARK_AT[1], SLOGAN[-1][2] + 7 * SLOGAN_CELL
    assert abs((top + bottom) / 2 - CENTRE) <= 1, f"text midline {(top + bottom) / 2}"

    ghost = load_ghost()
    used = [
        (c, r)
        for r, line in enumerate(ghost.grid())
        for c, cell in enumerate(line)
        if cell
    ]
    gx, gy = GHOST_AT
    left = gx + min(c for c, _ in used) * GHOST_CELL
    right = gx + (max(c for c, _ in used) + 1) * GHOST_CELL
    head = gy + min(r for _, r in used) * GHOST_CELL
    foot = gy + (max(r for _, r in used) + 1) * GHOST_CELL
    assert x + 8 <= left and right <= x + w - 8, f"sprite {left}..{right}, pane {x + 8}..{x + w - 8}"
    assert head < y + 8, f"head at {head} does not break the top frame"
    assert foot < y + h - 8, f"foot at {foot} sits on the bottom frame"

    for colour, dx, dy in DUST:
        assert 8 <= dy and dy + 4 <= H - 8, f"dust at {dy} is under a bar"
        assert not (x <= dx <= x + w and y <= dy <= y + h), f"dust at {dx},{dy} is in the window"

    stray = set(re.findall(r'fill="(#[0-9a-f]{6})"', out)) - (
        {WHITE, BG, PANEL, BAR, FRAME, CYAN, VIOLET}
        | {"#%02x%02x%02x" % rgb[:3] for rgb in ghost.PALETTE if rgb[3]}
    )
    assert not stray, f"colours outside the palette: {sorted(stray)}"

    root = re.search(r'shape-rendering="crispEdges"', out)
    assert root, "the banner must be drawn crisp"


def cells():
    """The sprite's own cells, for reading the art while moving it around."""
    return "\n".join(load_ghost().ART)


def main():
    if "--cells" in sys.argv:
        print(cells())
        return
    out = build()
    check(out)
    target = os.path.join(ROOT, "assets", "haunt-banner.svg")
    with open(target, "w") as handle:
        handle.write(out)
    print(f"{target}: {W}x{H}, {len(out)} bytes, ghost cell {GHOST_CELL}")


if __name__ == "__main__":
    main()
