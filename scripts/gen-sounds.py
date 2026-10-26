"""Synthesize the game's sound effects into assets/sounds/*.ogg.

Every sound is generated from noise and oscillators with a fixed seed, so
the files are reproducible:

    python scripts/gen-sounds.py

Needs numpy and ffmpeg (with libvorbis), found in PATH or at
C:/lib/ffmpeg/bin/ffmpeg.exe. assets/sounds.ron says which sound plays
when.
"""

import shutil
import subprocess
import sys
import tempfile
import wave
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "assets" / "sounds"
RATE = 44100
rng = np.random.default_rng(20261025)


def ffmpeg():
    exe = shutil.which("ffmpeg")
    if exe:
        return exe
    p = Path("C:/lib/ffmpeg/bin/ffmpeg.exe")
    if p.exists():
        return str(p)
    sys.exit("ffmpeg not found")


def t(seconds):
    return np.arange(int(seconds * RATE)) / RATE


def noise(seconds):
    return rng.standard_normal(int(seconds * RATE))


def lowpass(x, cutoff):
    """One-pole low-pass, run twice."""
    a = np.exp(-2 * np.pi * cutoff / RATE)
    for _ in range(2):
        y = np.empty_like(x)
        acc = 0.0
        for i, v in enumerate(x):
            acc = (1 - a) * v + a * acc
            y[i] = acc
        x = y
    return x


def highpass(x, cutoff):
    return x - lowpass(x, cutoff)


def env(n, attack, decay):
    """Attack then exponential decay, in seconds."""
    tt = np.arange(n) / RATE
    a = np.clip(tt / max(attack, 1e-4), 0, 1)
    return a * np.exp(-np.maximum(tt - attack, 0) / decay)


def ring(freqs, seconds, decay):
    tt = t(seconds)
    out = np.zeros_like(tt)
    for f, amp in freqs:
        out += amp * np.sin(2 * np.pi * f * tt) * np.exp(-tt / (decay * 800 / f))
    return out


def pad(x, seconds):
    return np.concatenate([x, np.zeros(int(seconds * RATE))])


def mix_at(base, x, at):
    i = int(at * RATE)
    end = min(len(base), i + len(x))
    base[i:end] += x[: end - i]
    return base


def normalize(x, peak=0.8):
    m = np.max(np.abs(x))
    return x if m == 0 else x * (peak / m)


def swing():
    n = noise(0.22)
    tt = t(0.22)
    # A whoosh: band-passed noise whose band sweeps up then down.
    lo = lowpass(n, 2200) - lowpass(n, 500)
    shape = np.sin(np.pi * tt / 0.22) ** 2
    return normalize(lo * shape, 0.35)


def hit_stone():
    click = highpass(noise(0.01), 3000) * env(int(0.01 * RATE), 0.0005, 0.003)
    body = ring([(1830, 1.0), (2710, 0.6), (4120, 0.35), (920, 0.3)], 0.25, 0.05)
    x = pad(click, 0.24)[: len(body)] + body * env(len(body), 0.001, 0.06)
    return normalize(x, 0.7)


def hit_soil():
    n = lowpass(noise(0.16), 700) * env(int(0.16 * RATE), 0.002, 0.04)
    thump = np.sin(2 * np.pi * 95 * t(0.16)) * env(int(0.16 * RATE), 0.002, 0.05)
    return normalize(n + 0.8 * thump, 0.7)


def hit_ore():
    return normalize(0.6 * hit_stone() + 0.5 * pad(hit_soil(), 0.09)[: len(hit_stone())], 0.7)


def hit_crystal():
    body = ring([(2637, 1.0), (3951, 0.7), (5274, 0.5), (1318, 0.3)], 0.6, 0.25)
    return normalize(body * env(len(body), 0.001, 0.2), 0.55)


def hit_plant():
    n = highpass(noise(0.18), 1800) * env(int(0.18 * RATE), 0.004, 0.05)
    return normalize(lowpass(n, 6000), 0.4)


def hit_liquid():
    tt = t(0.35)
    n = lowpass(noise(0.35), 1800) * env(len(tt), 0.005, 0.08)
    # Bubbles: short rising sine blips.
    out = n.copy()
    for k in range(6):
        at = rng.uniform(0.02, 0.2)
        f0 = rng.uniform(500, 1100)
        bt = t(0.05)
        blip = np.sin(2 * np.pi * (f0 + 4000 * bt) * bt) * env(len(bt), 0.002, 0.015)
        mix_at(out, 0.5 * blip, at)
    return normalize(out, 0.5)


def crumble(hit, count, spread, tail_cutoff):
    base = np.zeros(int((spread + 0.4) * RATE))
    for k in range(count):
        at = spread * (k / count) ** 1.6 + rng.uniform(0, 0.02)
        mix_at(base, hit() * rng.uniform(0.3, 1.0) * (1 - k / (count + 2)), at)
    tail = lowpass(noise(len(base) / RATE), tail_cutoff) * env(len(base), 0.01, 0.12)
    return normalize(base + 0.5 * tail, 0.8)


def place():
    thump = np.sin(2 * np.pi * 70 * t(0.2)) * env(int(0.2 * RATE), 0.002, 0.06)
    tap = lowpass(noise(0.2), 1500) * env(int(0.2 * RATE), 0.001, 0.02)
    return normalize(thump + 0.6 * tap, 0.7)


def step():
    n = lowpass(noise(0.12), 900) * env(int(0.12 * RATE), 0.006, 0.03)
    grit = highpass(noise(0.12), 2500) * env(int(0.12 * RATE), 0.004, 0.015)
    return normalize(n + 0.25 * grit, 0.35)


def season_chime():
    tt = t(2.2)
    out = np.zeros_like(tt)
    # A soft three-note bell: C6, E6, G6, staggered.
    for k, f in enumerate([1046.5, 1318.5, 1568.0]):
        at = 0.18 * k
        bt = t(2.2 - at)
        bell = (np.sin(2 * np.pi * f * bt) + 0.4 * np.sin(2 * np.pi * 2.76 * f * bt)
                ) * env(len(bt), 0.004, 0.6)
        mix_at(out, bell, at)
    return normalize(out, 0.35)


def wind(seconds=12.0):
    n = noise(seconds)
    # Gusts: noise low-passed at a cutoff that drifts, and a slow swell.
    a = lowpass(n, 350)
    b = lowpass(n, 900) - lowpass(n, 350)
    tt = t(seconds)
    swell = 0.6 + 0.4 * np.sin(2 * np.pi * tt / seconds) * np.sin(2 * np.pi * 3 * tt / seconds)
    gust = 0.5 + 0.5 * np.sin(2 * np.pi * 2 * tt / seconds + 1.0) ** 2
    x = a * swell + 0.6 * b * gust
    # Crossfade the ends so the loop has no seam.
    fade = int(1.0 * RATE)
    head, tail = x[:fade].copy(), x[-fade:].copy()
    w = np.linspace(0, 1, fade)
    x[:fade] = head * w + tail * (1 - w)
    x = x[: len(x) - fade]
    return normalize(x, 0.5)


SOUNDS = {
    "swing": swing,
    "hit_stone": hit_stone,
    "hit_soil": hit_soil,
    "hit_ore": hit_ore,
    "hit_crystal": hit_crystal,
    "hit_plant": hit_plant,
    "hit_liquid": hit_liquid,
    "break_stone": lambda: crumble(hit_stone, 7, 0.25, 900),
    "break_soil": lambda: crumble(hit_soil, 6, 0.2, 500),
    "break_ore": lambda: crumble(hit_ore, 7, 0.25, 800),
    "break_crystal": lambda: crumble(hit_crystal, 5, 0.3, 3000),
    "break_plant": lambda: crumble(hit_plant, 4, 0.15, 3000),
    "place": place,
    "step": step,
    "season": season_chime,
    "wind": wind,
}


def write_wav(path, x):
    pcm = (np.clip(x, -1, 1) * 32767).astype(np.int16)
    with wave.open(str(path), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(RATE)
        w.writeframes(pcm.tobytes())


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    exe = ffmpeg()
    with tempfile.TemporaryDirectory() as tmp:
        for name, make in SOUNDS.items():
            wav = Path(tmp) / f"{name}.wav"
            write_wav(wav, make())
            ogg = OUT / f"{name}.ogg"
            subprocess.run([exe, "-y", "-loglevel", "error", "-i", str(wav), "-c:a", "libvorbis",
                            "-q:a", "4", str(ogg)], check=True)
            print(f"{ogg.relative_to(ROOT)}: {ogg.stat().st_size} bytes")


if __name__ == "__main__":
    main()
