#!/usr/bin/env python3
"""Generate assets/ghost.png, the ghost shown in the launch banner.

The ghost is cell art on a small grid: each cell becomes a GRID x GRID block of
device pixels, so the figure stays crisp at any terminal cell size. The output
is an indexed PNG (one byte per pixel plus a palette), which keeps the embedded
bytes small.

The picture is written out as data instead of being computed from geometry, so
what is below is what ships, with two exceptions, both in grid(). The outline is
generated from the silhouette, in black, so that editing the art cannot leave a
hole in the border the way a hand-placed ring can; and the gaps the art seals
inside the figure are closed in the colour around them, because a transparent
speck in the middle of a solid ghost is not something anyone chose to draw. The
legend, for the art itself:

    . empty   1 mark (the eyes)   2 body   3 shadow

The ghost is copied from the reference, with its arm to the right and its tail
flowing down to the left. Regenerate after editing it:

    python3 tools/gen-ghost.py           # write assets/ghost.png
    python3 tools/gen-ghost.py --grid    # dump the grid as it ships, outline and all
    python3 tools/gen-ghost.py --svg     # dump the ghost as SVG paths

The PNG is committed, so builds need no Python. `--svg` prints the same grid as
`<path>` elements, one per colour, for pasting into assets/haunt-banner.svg.
"""

import os
import struct
import sys
import zlib
from collections import deque

GRID = 9  # device pixels per grid cell

PALETTE = (
    (0, 0, 0, 0),     # 0 empty
    (0, 0, 0, 255),   # 1 outline, and the marks: black, so it reads on any theme
    (249, 250, 251, 255),  # 2 body
    (179, 195, 228, 255),  # 3 shadow
)
EMPTY, OUTLINE, BODY, SHADOW = range(4)

ART = (
    ".............................",
    ".................111111......",
    "................12222221.....",
    "...............12222222211...",
    "..............1222222222221..",
    ".............122222222222221.",
    ".............122222211221221.",
    ".............122222.11221121.",
    "............1222222111221121.",
    "............1222222111221121.",
    "............1222222212222121.",
    "............1222222222222231.",
    "...........12222222222222231.",
    "..........122222222222222331.",
    "..........122222222222222331.",
    ".........1222222222222223331.",
    ".......112222222222222223331.",
    "......1222223222322223223331.",
    "..11112222233222322223233331.",
    ".12222222233322332223323.331.",
    ".122222233332223322233231331.",
    "..112223333222332222.333131..",
    "...11111333223332222133311...",
    ".......113222333223313311....",
    ".........122311333.1111......",
    "..........111..111111........",
    ".............................",
)

WIDTH = len(ART[0])   # grid columns, including the space the outline needs
HEIGHT = len(ART)     # grid rows

SYMBOL = {EMPTY: ".", OUTLINE: "#", BODY: "O", SHADOW: "x"}


def art():
    """The picture as drawn: the shape and its features, outline not yet added."""
    return [[EMPTY if cell == "." else int(cell) for cell in line] for line in ART]


def background(cells):
    """The empty cells the figure is seen against: those walkable from the edge.

    Two things are drawn from that distinction. The outline is generated on the
    cells touching this set, so it closes on the silhouette and not around
    every gap; and the empty cells that are not in it are pockets, which
    fill_pockets() shuts. Called on the art before that, it finds the pockets;
    called after, it is the surface the border is laid on.
    """
    seen = [[False] * WIDTH for _ in range(HEIGHT)]
    queue = deque(
        (row, column)
        for row in range(HEIGHT)
        for column in range(WIDTH)
        if not cells[row][column] and (row in (0, HEIGHT - 1) or column in (0, WIDTH - 1))
    )
    for row, column in queue:
        seen[row][column] = True
    while queue:
        row, column = queue.popleft()
        for dr, dc in ((1, 0), (-1, 0), (0, 1), (0, -1)):
            peer = (row + dr, column + dc)
            if (
                0 <= peer[0] < HEIGHT
                and 0 <= peer[1] < WIDTH
                and not seen[peer[0]][peer[1]]
                and not cells[peer[0]][peer[1]]
            ):
                seen[peer[0]][peer[1]] = True
                queue.append(peer)
    return seen


def fill_pockets(cells):
    """Close the gaps the art seals inside the figure.

    The hem is dithered on purpose, so most of its notches open onto the
    background and stay transparent; a few end up enclosed, and those would
    read as holes punched in the ghost once the outline closes over them. Each
    one takes the colour of the cells around it, ignoring the marks.
    """
    outside = background(cells)
    for row in range(HEIGHT):
        for column in range(WIDTH):
            if cells[row][column] or outside[row][column]:
                continue
            around = [
                cells[row + dr][column + dc]
                for dr in (-1, 0, 1)
                for dc in (-1, 0, 1)
                if 0 <= row + dr < HEIGHT
                and 0 <= column + dc < WIDTH
                and cells[row + dr][column + dc] not in (EMPTY, OUTLINE)
            ]
            cells[row][column] = max(set(around), key=around.count) if around else BODY
    return cells


def grid():
    """The picture as palette indices, one per cell, outline included.

    A cell is outline when any of its eight sides touches the background, so
    the border is one cell thick everywhere and cannot go missing where the art
    is redrawn. Marks the art already puts down, the eyes, are that colour.
    """
    cells = fill_pockets(art())
    outside = background(cells)
    for row in range(HEIGHT):
        for column in range(WIDTH):
            if not cells[row][column]:
                continue
            sides = (
                (row + dr, column + dc)
                for dr in (-1, 0, 1)
                for dc in (-1, 0, 1)
                if 0 <= row + dr < HEIGHT and 0 <= column + dc < WIDTH
            )
            if any(outside[r][c] for r, c in sides):
                cells[row][column] = OUTLINE
    return cells


def check():
    """Catch art that is not rectangular, off palette, or too close to the edge."""
    assert WIDTH and all(len(line) == WIDTH for line in ART), "ART is not rectangular"
    assert all(cell in ".123" for line in ART for cell in line), (
        "ART uses a character outside the palette"
    )
    # The outline is drawn on the silhouette, so the figure needs a cell of
    # clear grid around it or the border is cut off at that edge.
    assert all(cell == "." for cell in ART[0] + ART[-1]), "ART touches the grid's top or bottom"
    assert all(line[0] == line[-1] == "." for line in ART), "ART touches the grid's left or right"


def dump(cells):
    """The grid as text, for reading the art and the generated outline."""
    return "\n".join("".join(SYMBOL[cell] for cell in line) for line in cells)


def upscale(grid):
    """Turn each cell into a GRID x GRID block of device pixels."""
    return [[cell for cell in line for _ in range(GRID)] for line in grid for _ in range(GRID)]


def png(grid):
    """Encode the upscaled grid as an indexed PNG."""
    pixels = upscale(grid)
    height, width = len(pixels), len(pixels[0])

    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(
            ">I", zlib.crc32(body) & 0xFFFFFFFF
        )

    raw = b"".join(b"\x00" + bytes(line) for line in pixels)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 3, 0, 0, 0))
        + chunk(b"PLTE", b"".join(bytes(rgb[:3]) for rgb in PALETTE))
        + chunk(b"tRNS", bytes(rgb[3] for rgb in PALETTE))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def svg(grid, x=0, y=0, scale=1):
    """The grid as one SVG path per colour, as runs of cells."""
    out = []
    for index in range(1, len(PALETTE)):
        runs = []
        for row, line in enumerate(grid):
            column = 0
            while column < WIDTH:
                if line[column] != index:
                    column += 1
                    continue
                start = column
                while column < WIDTH and line[column] == index:
                    column += 1
                runs.append((start, row, column - start))
        if not runs:
            continue
        cells = "".join(
            f"M{col} {row}h{length}v1h-{length}z" for col, row, length in runs
        )
        colour = "#" + "".join(f"{part:02x}" for part in PALETTE[index][:3])
        out.append(f'<path fill="{colour}" d="{cells}"/>')
    body = "\n    ".join(out)
    return f'<g transform="translate({x} {y}) scale({scale})">\n    {body}\n</g>'


def main():
    check()
    picture = grid()
    if "--grid" in sys.argv:
        print(dump(picture))
    if "--svg" in sys.argv:
        print(svg(picture, 700, 55, 5.5))
        return

    assets = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "assets")
    os.makedirs(assets, exist_ok=True)
    out = os.path.join(assets, "ghost.png")
    data = png(picture)
    with open(out, "wb") as handle:
        handle.write(data)
    print(f"{out}: {WIDTH * GRID}x{HEIGHT * GRID}, {len(data)} bytes")


if __name__ == "__main__":
    main()
