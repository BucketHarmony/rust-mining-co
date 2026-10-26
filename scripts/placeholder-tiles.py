#!/usr/bin/env python3
"""Write placeholder tile textures into assets/tiles/ (seamless, 128 px).

Each tile is the material's color from assets/materials.ron with tileable
noise, so the tile pipeline has something to show before real art exists.
A file that already exists is left alone unless --force is given, so
hand-made or generated art is never overwritten.

    python scripts/placeholder-tiles.py [--force]
"""
import re
import sys
from pathlib import Path

import numpy as np
from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
SIZE = 128


def material_colors():
    text = (ROOT / "assets/materials.ron").read_text(encoding="utf-8")
    pat = re.compile(r'key: "([a-z_]+)".*?color: \((\d+), (\d+), (\d+), (\d+)\)', re.S)
    return {k: tuple(int(c) for c in rgba[:3]) for k, *rgba in pat.findall(text)}


def periodic_noise(rng, octaves):
    """Sum of random sinusoids with integer frequencies: tiles seamlessly."""
    y, x = np.mgrid[0:SIZE, 0:SIZE] / SIZE * 2 * np.pi
    out = np.zeros((SIZE, SIZE))
    for freq, amp in octaves:
        for _ in range(6):
            fx, fy = rng.integers(-freq, freq + 1, size=2)
            phase = rng.uniform(0, 2 * np.pi)
            out += amp * np.sin(fx * x + fy * y + phase)
    return out / np.abs(out).max()


def tile(color, seed, grain=0.18, speckle=0.0, strata=0.0, cracks=0.0):
    rng = np.random.default_rng(seed)
    base = np.array(color, dtype=float)
    n = periodic_noise(rng, [(2, 1.0), (5, 0.5), (11, 0.25)])
    shade = 1 + grain * n
    if strata:
        y = np.mgrid[0:SIZE, 0:SIZE][0] / SIZE * 2 * np.pi
        shade += strata * np.sin(3 * y + 0.6 * periodic_noise(rng, [(2, 1.0)]))
    if cracks:
        c = np.abs(periodic_noise(rng, [(3, 1.0), (7, 0.4)]))
        shade -= cracks * (c < 0.05)
    img = base * shade[..., None]
    if speckle:
        dots = rng.random((SIZE, SIZE)) < speckle
        img[dots] *= rng.uniform(0.6, 1.4, size=(dots.sum(), 1))
    return Image.fromarray(np.clip(img, 0, 255).astype(np.uint8))


def grass_side(grass, dirt, seed):
    rng = np.random.default_rng(seed)
    side = np.asarray(tile(dirt, seed, speckle=0.03), dtype=float)
    top = np.asarray(tile(grass, seed + 1), dtype=float)
    edge = 22 + 8 * periodic_noise(rng, [(3, 1.0), (8, 0.4)])[0]
    rows = np.arange(SIZE)[:, None]
    mask = rows < edge[None, :]
    side[mask] = top[mask]
    return Image.fromarray(side.astype(np.uint8))


def main():
    force = "--force" in sys.argv
    colors = material_colors()
    out = ROOT / "assets/tiles"
    out.mkdir(parents=True, exist_ok=True)
    tiles = {
        "topsoil_top.png": lambda: tile(colors["topsoil"], 1, grain=0.22, speckle=0.05),
        "topsoil_side.png": lambda: grass_side(colors["topsoil"], colors["dirt"], 2),
        "dirt.png": lambda: tile(colors["dirt"], 3, speckle=0.04),
        "clay.png": lambda: tile(colors["clay"], 4, grain=0.1, strata=0.05),
        "stone.png": lambda: tile(colors["stone"], 5, cracks=0.25),
        "granite.png": lambda: tile(colors["granite"], 6, grain=0.1, speckle=0.2),
        "basalt.png": lambda: tile(colors["basalt"], 7, cracks=0.3),
        "limestone.png": lambda: tile(colors["limestone"], 8, grain=0.08, strata=0.08),
        "coal_seam.png": lambda: tile(colors["coal_seam"], 9, grain=0.3, speckle=0.1),
        "sulfide_ore.png": lambda: tile(colors["sulfide_ore"], 10, cracks=0.2, speckle=0.1),
        "iron_ore.png": lambda: tile(colors["iron_ore"], 11, speckle=0.15),
        "bedrock.png": lambda: tile(colors["bedrock"], 12, grain=0.3, cracks=0.4),
    }
    for name, make in tiles.items():
        path = out / name
        if path.exists() and not force:
            print("keep", path.relative_to(ROOT))
            continue
        make().save(path)
        print("wrote", path.relative_to(ROOT))


if __name__ == "__main__":
    main()
