#!/usr/bin/env python3
"""Preview tiles repeated 3 x 3, to check that they are seamless.

    python scripts/tile-preview.py [assets/tiles/stone.png ...]

With no arguments, previews every PNG in assets/tiles/. Writes
target/tile-preview/<name>.png (each tile 3 x 3 at 256 px per tile). A tile
that is not seamless shows a grid of lines where its edges meet.
"""
import sys
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
CELL = 256


def main():
    files = [Path(a) for a in sys.argv[1:]] or sorted((ROOT / "assets/tiles").glob("*.png"))
    out = ROOT / "target/tile-preview"
    out.mkdir(parents=True, exist_ok=True)
    for f in files:
        tile = Image.open(f).convert("RGB").resize((CELL, CELL), Image.LANCZOS)
        sheet = Image.new("RGB", (3 * CELL, 3 * CELL))
        for i in range(3):
            for j in range(3):
                sheet.paste(tile, (i * CELL, j * CELL))
        path = out / f.name
        sheet.save(path)
        print(path)


if __name__ == "__main__":
    main()
