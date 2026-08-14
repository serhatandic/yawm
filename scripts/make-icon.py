#!/usr/bin/env python3
"""
Generate yawm's app icon.

macOS does not mask app icons — every icon draws its own shape — so a
full-bleed square sits in the dock looking broken next to a row of squircles.
The geometry below is the platform's, not a guess:

    canvas   1024x1024, transparent
    shape     824x824 centred, leaving ~100px clear on every side
    radius    ~185 on that shape

That margin is not decoration. It is what makes the icon sit at the same
visual weight as its neighbours.

The mark is the app's one idea: stacked rows standing for worktrees, the top
one green for disposable. Windows and Linux take the same source; only macOS
needs the inset, and having it costs the others nothing.

Usage:
    python3 scripts/make-icon.py
    npx tauri icon apps/desktop/src-tauri/icons/icon.png
"""

from __future__ import annotations

import pathlib

from PIL import Image, ImageDraw

SIZE = 1024
INSET = 100
RADIUS = 185
# Supersample, then downscale, so the squircle and the rounded rows come out
# smooth. PIL's rounded_rectangle is aliased at final size.
SCALE = 4

BACKDROP = (24, 24, 27, 255)      # zinc-900, matching the app's --color-background
ROW_IDLE = (228, 228, 231, 255)   # zinc-200
ROW_LIVE = (74, 222, 128, 255)    # the disposable green
DOT_DIM = (113, 113, 122, 255)    # zinc-500

OUT = (
    pathlib.Path(__file__).resolve().parent.parent
    / "apps/desktop/src-tauri/icons/icon.png"
)


def main() -> None:
    s = SCALE
    canvas = Image.new("RGBA", (SIZE * s, SIZE * s), (0, 0, 0, 0))
    draw = ImageDraw.Draw(canvas)

    # The squircle the icon lives inside.
    box = (INSET * s, INSET * s, (SIZE - INSET) * s, (SIZE - INSET) * s)
    draw.rounded_rectangle(box, radius=RADIUS * s, fill=BACKDROP)

    # Three rows: a worktree list. Proportions are derived from the shape so
    # changing the inset keeps the mark centred.
    shape = (SIZE - INSET * 2) * s
    left = INSET * s
    top = INSET * s

    dot_r = int(shape * 0.035)
    dot_x = left + int(shape * 0.235)
    row_x0 = left + int(shape * 0.36)
    row_x1 = left + int(shape * 0.80)
    row_h = int(shape * 0.115)
    gap = int(shape * 0.085)
    block = row_h * 3 + gap * 2
    row_y = top + (shape - block) // 2

    for index in range(3):
        y0 = row_y + index * (row_h + gap)
        colour = ROW_LIVE if index == 0 else ROW_IDLE
        draw.rounded_rectangle(
            (row_x0, y0, row_x1, y0 + row_h),
            radius=row_h // 2,
            fill=colour,
        )
        cy = y0 + row_h // 2
        draw.ellipse(
            (dot_x - dot_r, cy - dot_r, dot_x + dot_r, cy + dot_r),
            fill=ROW_LIVE if index == 0 else DOT_DIM,
        )

    icon = canvas.resize((SIZE, SIZE), Image.LANCZOS)
    OUT.parent.mkdir(parents=True, exist_ok=True)
    icon.save(OUT)
    print(f"wrote {OUT} ({SIZE}x{SIZE}, transparent, {INSET}px inset)")


if __name__ == "__main__":
    main()
