#!/usr/bin/env python3
"""Generate seamless tile textures with a local ComfyUI (SDXL), via its API.

    python scripts/comfy-tiles.py [names...] [--candidates N] [--pick name=K ...]
                                  [--server http://127.0.0.1:8188] [--ckpt FILE]

For each tile name (default: every entry in TILES below) it makes N candidates
in target/tile-candidates/<name>-<k>.png, and a contact sheet
target/tile-candidates/<name>-sheet.png showing each candidate repeated 2 x 2.
Nothing is written to assets/tiles/ unless asked: `--pick name=K` copies
candidate K, and `--write` copies candidate 0 of every generated name.

Seamless, with stock ComfyUI nodes only:
  1. txt2img at 1024 x 1024.
  2. Roll the image by half its size, so its old edges meet in a cross through
     the middle; the rolled image's own edges are the old middle, so they
     already wrap.
  3. Inpaint the cross (a feathered band) with img2img on a latent noise mask,
     and composite the result back through the feathered mask.
Derived tiles (topsoil_side) are composed from generated ones in Python.
"""
import argparse
import io
import json
import shutil
import sys
import time
import urllib.parse
import urllib.request
import uuid
from pathlib import Path

import numpy as np
from PIL import Image, ImageFilter

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "target/tile-candidates"
TILES_DIR = ROOT / "assets/tiles"
SIZE = 1024
# Half-width of the inpainted seam band, and its feather, in pixels.
SEAM_HALF = 96
SEAM_FEATHER = 40

STYLE = (
    "seamless tileable texture, {subject}, flat even diffuse lighting, no shadows, "
    "top-down orthographic view, hand-painted fantasy game texture, albedo map, "
    "stylized, rich detail, sharp focus"
)
NEGATIVE = (
    "perspective, horizon, sky, cast shadow, vignette, border, frame, text, watermark, "
    "signature, logo, person, blurry, lowres, jpeg artifacts, tiling seams, lens flare"
)

TILES = {
    "topsoil_top": "lush short grass and clover seen from directly above, small leaves",
    "dirt": "brown earth soil with small pebbles and thin roots",
    "clay": "smooth reddish-brown clay with fine cracks and faint horizontal layers",
    "stone": "rough grey natural rock face, fractured stone surface with cracks, ridges and mineral grain",
    "granite": "pink and grey granite with coarse speckled crystals",
    "basalt": "dark blue-grey basalt rock with small vesicles and joints",
    "limestone": "pale cream limestone with sedimentary bands and tiny shell fossils",
    "coal_seam": "black coal seam with glossy faceted surfaces in dark rock",
    "sulfide_ore": "grey rock with brassy golden copper sulfide veins and crystals",
    "iron_ore": "rusty red-brown rock with dark iron ore nodules",
    "glowcrystal": "cluster of glowing cyan crystals embedded in dark rock",
    "bedrock": "very dark dense fractured stone, almost black",
    "brass_plate": "riveted brass metal panel, steampunk, worn polished edges",
    "ice": "blue-white solid ice with trapped bubbles and cracks",
    "obsidian": "black volcanic glass with conchoidal fractures and purple sheen",
    "magma": "glowing orange-red molten lava with dark cooling crust cracks",
    "crushed_sulfide": "gravel of crushed brassy golden ore chunks",
    "copper_dust": "heaped fine green-blue copper powder",
    "iron_dust": "heaped fine dark grey iron filings powder",
    "salt_dust": "heaped fine white salt powder",
    "ochre_crust": "orange-yellow iron oxide mineral crust, rough",
    "salt_crust": "white crystalline salt crust with small cubic crystals",
}

# Extra negative terms; solid materials must not turn into cobbles or bricks.
SOLID_NEG = "cobblestone, pebbles, boulders, pavement, bricks, tiles, masonry, stone wall, gravel"
EXTRA_NEG = {n: SOLID_NEG for n in [
    "stone", "granite", "basalt", "limestone", "bedrock", "obsidian", "clay", "ice",
    "coal_seam", "sulfide_ore", "iron_ore", "glowcrystal",
]}

# Tiles composed from others: name -> (function, inputs).
DERIVED = {"topsoil_side": ("grass_side", ["topsoil_top", "dirt"])}


class Comfy:
    def __init__(self, server):
        self.server = server.rstrip("/")
        self.client = uuid.uuid4().hex

    def _get(self, path):
        with urllib.request.urlopen(self.server + path, timeout=60) as r:
            return r.read()

    def upload(self, img, name):
        boundary = uuid.uuid4().hex
        buf = io.BytesIO()
        img.save(buf, "PNG")
        body = (
            f'--{boundary}\r\nContent-Disposition: form-data; name="image"; filename="{name}"\r\n'
            f"Content-Type: image/png\r\n\r\n"
        ).encode() + buf.getvalue() + (
            f'\r\n--{boundary}\r\nContent-Disposition: form-data; name="overwrite"\r\n\r\ntrue'
            f"\r\n--{boundary}--\r\n"
        ).encode()
        req = urllib.request.Request(
            self.server + "/upload/image",
            data=body,
            headers={"Content-Type": f"multipart/form-data; boundary={boundary}"},
        )
        with urllib.request.urlopen(req, timeout=60) as r:
            return json.load(r)["name"]

    def run(self, graph):
        req = urllib.request.Request(
            self.server + "/prompt",
            data=json.dumps({"prompt": graph, "client_id": self.client}).encode(),
            headers={"Content-Type": "application/json"},
        )
        with urllib.request.urlopen(req, timeout=60) as r:
            pid = json.load(r)["prompt_id"]
        while True:
            hist = json.loads(self._get(f"/history/{pid}"))
            if pid in hist:
                entry = hist[pid]
                status = entry.get("status", {})
                if status.get("status_str") == "error":
                    sys.exit(f"ComfyUI error: {json.dumps(status)[:800]}")
                for out in entry["outputs"].values():
                    for im in out.get("images", []):
                        q = urllib.parse.urlencode(im)
                        return Image.open(io.BytesIO(self._get(f"/view?{q}"))).convert("RGB")
                sys.exit(f"no image in ComfyUI output: {entry['outputs']}")
            time.sleep(0.5)


def base_nodes(ckpt, prompt, seed, negative=NEGATIVE):
    return {
        "ckpt": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": ckpt}},
        "pos": {"class_type": "CLIPTextEncode", "inputs": {"text": prompt, "clip": ["ckpt", 1]}},
        "neg": {"class_type": "CLIPTextEncode", "inputs": {"text": negative, "clip": ["ckpt", 1]}},
        "sample": {
            "class_type": "KSampler",
            "inputs": {
                "model": ["ckpt", 0],
                "positive": ["pos", 0],
                "negative": ["neg", 0],
                "seed": seed,
                "steps": 30,
                "cfg": 6.0,
                "sampler_name": "dpmpp_2m",
                "scheduler": "karras",
                "denoise": 1.0,
            },
        },
        "decode": {"class_type": "VAEDecode", "inputs": {"samples": ["sample", 0], "vae": ["ckpt", 2]}},
        "save": {"class_type": "SaveImage", "inputs": {"images": ["decode", 0], "filename_prefix": "rmc_tile"}},
    }


def txt2img(comfy, ckpt, prompt, negative, seed):
    g = base_nodes(ckpt, prompt, seed, negative)
    g["latent"] = {"class_type": "EmptyLatentImage", "inputs": {"width": SIZE, "height": SIZE, "batch_size": 1}}
    g["sample"]["inputs"]["latent_image"] = ["latent", 0]
    return comfy.run(g)


def seam_mask():
    """White cross through the middle, feathered."""
    m = np.zeros((SIZE, SIZE), np.uint8)
    c = SIZE // 2
    m[c - SEAM_HALF : c + SEAM_HALF, :] = 255
    m[:, c - SEAM_HALF : c + SEAM_HALF] = 255
    return Image.fromarray(m).filter(ImageFilter.GaussianBlur(SEAM_FEATHER / 2))


def make_seamless(comfy, ckpt, prompt, negative, seed, img):
    rolled = Image.fromarray(np.roll(np.asarray(img), (SIZE // 2, SIZE // 2), axis=(0, 1)))
    mask = seam_mask()
    tag = uuid.uuid4().hex[:8]
    img_name = comfy.upload(rolled, f"rmc_seam_{tag}.png")
    mask_name = comfy.upload(mask.convert("RGB"), f"rmc_mask_{tag}.png")
    g = base_nodes(ckpt, prompt, seed + 1, negative)
    g["img"] = {"class_type": "LoadImage", "inputs": {"image": img_name}}
    g["mask"] = {"class_type": "LoadImageMask", "inputs": {"image": mask_name, "channel": "red"}}
    g["enc"] = {"class_type": "VAEEncode", "inputs": {"pixels": ["img", 0], "vae": ["ckpt", 2]}}
    g["noise_mask"] = {"class_type": "SetLatentNoiseMask", "inputs": {"samples": ["enc", 0], "mask": ["mask", 0]}}
    g["sample"]["inputs"]["latent_image"] = ["noise_mask", 0]
    g["sample"]["inputs"]["denoise"] = 0.75
    fixed = comfy.run(g)
    a = np.asarray(mask, dtype=float)[..., None] / 255
    out = np.asarray(rolled, dtype=float) * (1 - a) + np.asarray(fixed, dtype=float) * a
    return flatten(Image.fromarray(out.round().astype(np.uint8)))


def flatten(img, sigma=SIZE // 6):
    """Divide out low-frequency brightness (wrapping), so the tile is evenly lit
    and the inpainted cross does not show as a lighter or darker band."""
    a = np.asarray(img, dtype=float)
    big = Image.fromarray(np.tile(a, (3, 3, 1)).round().astype(np.uint8))
    blur = np.asarray(big.filter(ImageFilter.GaussianBlur(sigma)), dtype=float)[SIZE : 2 * SIZE, SIZE : 2 * SIZE]
    luma = lambda x: x @ [0.2126, 0.7152, 0.0722]
    gain = luma(a).mean() / np.maximum(luma(blur), 1)
    return Image.fromarray(np.clip(a * gain[..., None], 0, 255).round().astype(np.uint8))


def grass_side(grass, dirt):
    """Dirt with a ragged band of the grass tile along the top edge."""
    rng = np.random.default_rng(7)
    g, d = np.asarray(grass, dtype=float), np.asarray(dirt, dtype=float).copy()
    x = np.arange(SIZE) / SIZE * 2 * np.pi
    edge = SIZE * 0.16 + sum(
        SIZE * a * np.sin(f * x + rng.uniform(0, 6.28)) for f, a in [(2, 0.02), (5, 0.015), (13, 0.01)]
    )
    rows = np.arange(SIZE)[:, None]
    t = np.clip((edge[None, :] - rows) / 6 + 0.5, 0, 1)[..., None]
    shadow = 1 - 0.35 * np.clip(1 - (rows - edge[None, :]) / (SIZE * 0.04), 0, 1)[..., None] * (1 - t)
    return Image.fromarray((g * t + d * (1 - t) * shadow).round().astype(np.uint8))


def sheet(images):
    """Each candidate repeated 2 x 2 at 384 px, side by side."""
    cell = 192
    out = Image.new("RGB", (len(images) * cell * 2 + (len(images) - 1) * 16, cell * 2), "white")
    for k, img in enumerate(images):
        small = img.resize((cell, cell), Image.LANCZOS)
        for i in range(2):
            for j in range(2):
                out.paste(small, (k * (cell * 2 + 16) + i * cell, j * cell))
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("names", nargs="*")
    ap.add_argument("--candidates", type=int, default=3)
    ap.add_argument("--seed", type=int, default=1000)
    ap.add_argument("--server", default="http://127.0.0.1:8188")
    ap.add_argument("--ckpt", default="juggernautXL_v8Rundiffusion.safetensors")
    ap.add_argument("--write", action="store_true", help="copy candidate 0 of each generated tile")
    ap.add_argument("--pick", nargs="*", default=[], help="name=K: copy candidate K to assets/tiles/")
    args = ap.parse_args()
    OUT.mkdir(parents=True, exist_ok=True)

    picks = dict(p.split("=") for p in args.pick)
    if picks and not args.names:
        for name, k in picks.items():
            src = OUT / f"{name}-{k}.png"
            shutil.copy(src, TILES_DIR / f"{name}.png")
            print("picked", src.name, "->", f"assets/tiles/{name}.png")
        return

    comfy = Comfy(args.server)
    names = args.names or list(TILES) + list(DERIVED)
    for name in names:
        if name in DERIVED:
            fn, inputs = DERIVED[name]
            srcs = [TILES_DIR / f"{n}.png" for n in inputs]
            imgs = [Image.open(p).convert("RGB").resize((SIZE, SIZE), Image.LANCZOS) for p in srcs]
            out = globals()[fn](*imgs)
            out.save(OUT / f"{name}-0.png")
            sheet([out]).save(OUT / f"{name}-sheet.png")
            print("derived", name, "from", ", ".join(inputs))
            if args.write:
                shutil.copy(OUT / f"{name}-0.png", TILES_DIR / f"{name}.png")
            continue
        prompt = STYLE.format(subject=TILES[name])
        negative = ", ".join(filter(None, [NEGATIVE, EXTRA_NEG.get(name)]))
        made = []
        for k in range(args.candidates):
            seed = args.seed + 7919 * k
            t0 = time.time()
            raw = txt2img(comfy, args.ckpt, prompt, negative, seed)
            tile = make_seamless(comfy, args.ckpt, prompt, negative, seed, raw)
            tile.save(OUT / f"{name}-{k}.png")
            made.append(tile)
            print(f"{name}-{k}: {time.time() - t0:.0f}s", flush=True)
        sheet(made).save(OUT / f"{name}-sheet.png")
        if args.write:
            shutil.copy(OUT / f"{name}-0.png", TILES_DIR / f"{name}.png")


if __name__ == "__main__":
    main()
