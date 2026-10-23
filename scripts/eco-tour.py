"""Assemble the ecology video tour from the frames `eco_tour` saved.

Reads target/e2e/eco_tour/<segment>/manifest.tsv in tour order, burns a
caption bar into every frame, draws the title cards, and encodes an H.264
MP4 with ffmpeg.

    python scripts/eco-tour.py [--out target/video/eco_tour.mp4] [--fps 30]

Manifest rows (tab separated):
    title  <title>  <subtitle>  <seconds>
    frame  <file>   <caption>
    hold   <file>   <caption>   <seconds>
"""

import argparse
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent
FRAMES = ROOT / "target" / "e2e" / "eco_tour"
# The order of SEGMENTS in tests/e2e/tests/eco_tour.rs.
ORDER = ["intro", "water", "seepage", "meadow", "tailings", "limestone",
         "freeze", "magma", "firedamp"]
FADE_S = 0.5
BAR = (16, 15, 13, 190)
INK = (243, 238, 229)
ACCENT = (224, 173, 74)


def font(names, size):
    for name in names:
        for d in [Path("C:/Windows/Fonts"), Path("/usr/share/fonts/truetype/dejavu")]:
            if (d / name).exists():
                return ImageFont.truetype(str(d / name), size)
    return ImageFont.load_default(size)


def ffmpeg():
    exe = shutil.which("ffmpeg")
    if exe:
        return exe
    for p in [Path("C:/lib/ffmpeg/bin/ffmpeg.exe")]:
        if p.exists():
            return str(p)
    sys.exit("ffmpeg not found")


class Writer:
    def __init__(self, out_dir, size, fps):
        self.dir, self.size, self.fps, self.n = out_dir, size, fps, 0
        h = size[1]
        self.caption_font = font(["segoeui.ttf", "DejaVuSans.ttf"], round(h * 0.034))
        self.chip_font = font(["segoeuib.ttf", "DejaVuSans-Bold.ttf"], round(h * 0.024))
        self.title_font = font(["segoeuib.ttf", "DejaVuSans-Bold.ttf"], round(h * 0.085))
        self.sub_font = font(["segoeui.ttf", "DejaVuSans.ttf"], round(h * 0.036))

    def put(self, im):
        im.convert("RGB").save(self.dir / f"{self.n:06d}.png", compress_level=1)
        self.n += 1

    def caption(self, im, segment, text):
        im = im.convert("RGBA").resize(self.size, Image.LANCZOS)
        w, h = self.size
        layer = Image.new("RGBA", self.size, (0, 0, 0, 0))
        d = ImageDraw.Draw(layer)
        pad = round(h * 0.022)
        bar_h = round(h * 0.09)
        d.rectangle([0, h - bar_h, w, h], fill=BAR)
        chip = segment.upper()
        cw = d.textlength(chip, font=self.chip_font)
        cy = h - bar_h / 2
        d.rounded_rectangle([pad, cy - h * 0.022, pad + cw + 2 * pad * 0.6, cy + h * 0.022],
                            radius=4, fill=ACCENT)
        d.text((pad + pad * 0.6, cy), chip, font=self.chip_font, fill=(27, 26, 24), anchor="lm")
        d.text((pad * 2.2 + cw + pad, cy), text, font=self.caption_font, fill=INK, anchor="lm")
        return Image.alpha_composite(im, layer)

    def title(self, title, subtitle, seconds, background=None):
        w, h = self.size
        if background is None:
            base = Image.new("RGBA", self.size, (20, 19, 17, 255))
        else:
            base = background.convert("RGBA").resize(self.size, Image.LANCZOS)
            base = Image.alpha_composite(base, Image.new("RGBA", self.size, (16, 15, 13, 200)))
        d = ImageDraw.Draw(base)
        d.text((w / 2, h * 0.44), title, font=self.title_font, fill=INK, anchor="ms")
        d.line([w / 2 - w * 0.05, h * 0.485, w / 2 + w * 0.05, h * 0.485], fill=ACCENT, width=3)
        d.text((w / 2, h * 0.56), subtitle, font=self.sub_font, fill=(200, 194, 184), anchor="ms")
        n = round(seconds * self.fps)
        fade = round(FADE_S * self.fps)
        black = Image.new("RGBA", self.size, (0, 0, 0, 255))
        for i in range(n):
            a = min(1.0, (i + 1) / fade, (n - i) / fade)
            self.put(Image.blend(black, base, a))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=str(ROOT / "target" / "video" / "eco_tour.mp4"))
    ap.add_argument("--fps", type=int, default=30)
    ap.add_argument("--crf", type=int, default=20)
    args = ap.parse_args()

    segments = [s for s in ORDER if (FRAMES / s / "manifest.tsv").exists()]
    if not segments:
        sys.exit(f"no frames under {FRAMES}; run scripts/e2e.sh eco_tour first")
    first = next(iter(sorted((FRAMES / segments[0]).glob("*.png"))))
    size = Image.open(first).size

    tmp = Path(tempfile.mkdtemp(prefix="eco_tour_"))
    try:
        wr = Writer(tmp, size, args.fps)
        for seg in segments:
            rows = [l.rstrip("\n").split("\t")
                    for l in (FRAMES / seg / "manifest.tsv").read_text(encoding="utf-8").splitlines()
                    if l.strip()]
            # A title card over the segment's first frame, darkened.
            first_frame = next((r[1] for r in rows if r[0] in ("frame", "hold")), None)
            for r in rows:
                kind = r[0]
                if kind == "title":
                    bg = Image.open(FRAMES / seg / first_frame) if first_frame else None
                    wr.title(r[1], r[2], float(r[3]), bg)
                elif kind == "frame":
                    wr.put(wr.caption(Image.open(FRAMES / seg / r[1]), seg, r[2]))
                elif kind == "hold":
                    im = wr.caption(Image.open(FRAMES / seg / r[1]), seg, r[2])
                    for _ in range(round(float(r[3]) * args.fps)):
                        wr.put(im)
            print(f"{seg}: total {wr.n} frames", flush=True)
        out = Path(args.out)
        out.parent.mkdir(parents=True, exist_ok=True)
        subprocess.run([ffmpeg(), "-y", "-loglevel", "error", "-framerate", str(args.fps),
                        "-i", str(tmp / "%06d.png"), "-c:v", "libx264", "-preset", "slow",
                        "-crf", str(args.crf), "-pix_fmt", "yuv420p", "-movflags", "+faststart",
                        str(out)], check=True)
        print(f"{out}: {wr.n} frames, {wr.n / args.fps:.1f} s, {out.stat().st_size / 1e6:.1f} MB")
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    main()
