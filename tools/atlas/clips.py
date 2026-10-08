"""Generate the dual-grid transition clips (docs/PLAN.md, M3).

The M2 recipe (collage -> harmonize -> stitch, see tiles.py), extended to
time. Each cell plays one of 5 transitions over a sim step, so a tile is one
of 5^4 = 625 clips:

1. Worlds: a few small grids of transitions that together contain every 2x2
   configuration (one world is too big for one video pass).
2. Gen: per world, an animated collage of keyframe sprites (a sapling scaling
   up, the v0 ignition clip, flames dying into ground), harmonized with one
   SDEdit video pass of Wan VACE. Raw frames are saved per world, so the
   stage resumes after an interruption.
3. Stitch: border pieces are keyed by transitions, exactly as for stills,
   and one leader occurrence per key (the medoid over every world, all
   frames together) is pasted over all the others in every frame.
4. Pin: the first frames are blended into the stitched still world of the
   previous states and the last frames into that of the current states, so
   each clip starts and ends on exact M2 tiles.
5. Pack: one 25x25 page of tiles per frame, written to $TINDERBOX_DATA/atlas.

Usage: python clips.py [worlds|gen|pack|all] [--probe]
"""

from __future__ import annotations

import argparse
import json
import logging
import random
from collections import defaultdict

import numpy as np
from PIL import Image, ImageDraw, ImageFilter

import tiles
import turnstile
from generate import DATA, NEGATIVE, embed, pipe
from tiles import B, C, F as FEATHER, PROMPT, STEPS, VAE, encode, pieces

WORK = DATA / "work" / "clips3"
OUT = DATA / "atlas"

N = 8  # world size in cells
FRAMES = 17  # Wan wants 4k + 1
OUT_TILE = 64  # tiles are generated at C px and stored at this size
GUTTER = 4  # px of edge padding around each stored tile, for mip filtering
PIN = 4  # frames over which the ends blend into the stills

# Per-cell transitions over one step: (previous, current) state.
EMPTY, TREE, FIRE = tiles.EMPTY, tiles.TREE, tiles.FIRE
TRANSITIONS = {
    "empty": (EMPTY, EMPTY),
    "tree": (TREE, TREE),
    "grow": (EMPTY, TREE),
    "catch": (TREE, FIRE),  # spread or lightning: the cell's own corner can't know which
    "burnout": (FIRE, EMPTY),
}
K = len(TRANSITIONS)
PREV = np.array([p for p, _ in TRANSITIONS.values()])
CUR = np.array([c for _, c in TRANSITIONS.values()])
CONFIGS = K ** 4

CLIP_PROMPT = PROMPT + ", static camera, flames flicker, smoke drifts"

log = logging.getLogger("clips")


# --- Worlds ----------------------------------------------------------------

def config(g: np.ndarray, vx: int, vy: int, k: int = K) -> int:
    nw, ne, sw, se = g[vy - 1, vx - 1], g[vy - 1, vx], g[vy, vx - 1], g[vy, vx]
    return int(sw + k * se + k * k * nw + k ** 3 * ne)


def windows(g: np.ndarray) -> set[int]:
    return {config(g, vx, vy) for vy in range(1, N) for vx in range(1, N)}


def make_worlds(seed: int = 0) -> list[np.ndarray]:
    """Greedy cover: each new world is annealed to contain as many still
    missing configurations as possible."""
    rnd = random.Random(seed)
    covered: set[int] = set()
    worlds = []
    while len(covered) < CONFIGS:
        g = np.array([[rnd.randrange(K) for _ in range(N)] for _ in range(N)])
        score = lambda: len(windows(g) - covered)
        s, temp = score(), 1.0
        for _ in range(30000):
            x, y = rnd.randrange(N), rnd.randrange(N)
            old = g[y, x]
            g[y, x] = rnd.randrange(K)
            s2 = score()
            if s2 >= s or rnd.random() < np.exp((s2 - s) / temp):
                s = s2
            else:
                g[y, x] = old
            temp = max(0.05, temp * 0.9998)
        covered |= windows(g)
        worlds.append(g.copy())
        log.info("world %d adds %d configs, %d/%d covered", len(worlds) - 1, s, len(covered), CONFIGS)
    return worlds


# --- Collage ---------------------------------------------------------------

def ease(t: float) -> float:
    t = min(1.0, max(0.0, t))
    return t * t * (3 - 2 * t)


class Sprites:
    """Per-transition sprite frames for the collage. The ends match the
    still collage (tiles.collage) exactly: ground, tree disc or fire disc."""

    def __init__(self):
        kf = DATA / "work" / "keyframes"
        v0 = DATA / "work" / "clips"
        self.d = d = int(C / 0.75 * 0.9)
        self.ground = Image.open(kf / "empty.png").convert("RGB").resize((2 * C, 2 * C), Image.LANCZOS)
        disc = Image.new("L", (d, d))
        ImageDraw.Draw(disc).ellipse([d * 0.12, d * 0.12, d * 0.88, d * 0.88], fill=255)
        self.disc = np.asarray(disc.filter(ImageFilter.GaussianBlur(d // 30)), np.float32) / 255
        load = lambda p: np.asarray(Image.open(p).convert("RGB").resize((d, d), Image.LANCZOS), np.float32)
        self.tree, self.fire = load(kf / "tree.png"), load(kf / "fire.png")
        self.ignite = [load(v0 / "lightning" / f"{i:03d}.png") for i in range(33)]
        self.burn = [load(v0 / "burnout" / f"{i:03d}.png") for i in range(12)]

    def frame(self, tr: str, t: float):
        """(rgb, alpha) for one transition at time t in [0, 1], or None."""
        if tr == "empty":
            return None
        if tr == "tree":
            return self.tree, self.disc
        if tr == "grow":
            # A sapling scaling up; the v0 grow clip pops in a single frame.
            s = 0.25 + 0.75 * ease(t)
            k = max(1, round(self.d * s))
            o = (self.d - k) // 2
            rgb = np.zeros_like(self.tree)
            a = np.zeros_like(self.disc)
            rgb[o:o + k, o:o + k] = np.asarray(Image.fromarray(self.tree.astype(np.uint8)).resize((k, k), Image.LANCZOS))
            a[o:o + k, o:o + k] = np.asarray(Image.fromarray((self.disc * 255).astype(np.uint8)).resize((k, k), Image.LANCZOS)) / 255
            return rgb, a * ease(t * 3)
        if tr == "catch":
            # v0 ignition, skipping its idle start, eased into the keyframes at both ends.
            rgb = self.ignite[round(10 + 22 * t)]
            rgb = mix(rgb, self.tree, max(0.0, 1 - t / 0.15))
            rgb = mix(rgb, self.fire, max(0.0, (t - 0.8) / 0.2))
            return rgb, self.disc
        if tr == "burnout":
            rgb = mix(self.burn[round(11 * min(1.0, t / 0.6))], self.fire, max(0.0, 1 - t / 0.1))
            return rgb, self.disc * (1 - ease((t - 0.35) / 0.65))
        raise ValueError(tr)


def mix(a: np.ndarray, b: np.ndarray, w: float) -> np.ndarray:
    return a * (1 - w) + b * w


def collage_video(g: np.ndarray, sp: Sprites) -> list[Image.Image]:
    names = list(TRANSITIONS)
    W = N * C
    base = np.zeros((W, W, 3), np.float32)
    gr = np.asarray(sp.ground, np.float32)
    for y in range(0, W, 2 * C):
        for x in range(0, W, 2 * C):
            base[y:y + 2 * C, x:x + 2 * C] = gr[:W - y, :W - x]
    d = sp.d
    pad = d
    frames = []
    for f in range(FRAMES):
        t = f / (FRAMES - 1)
        im = np.pad(base, ((pad, pad), (pad, pad), (0, 0)))
        for y in range(N):
            for x in range(N):
                spr = sp.frame(names[g[y, x]], t)
                if spr is None:
                    continue
                rgb, a = spr
                y0 = pad + y * C + C // 2 - d // 2
                x0 = pad + x * C + C // 2 - d // 2
                a = a[..., None]
                im[y0:y0 + d, x0:x0 + d] = im[y0:y0 + d, x0:x0 + d] * (1 - a) + rgb * a
        frames.append(Image.fromarray(im[pad:pad + W, pad:pad + W].round().clip(0, 255).astype(np.uint8)))
    return frames


# --- Diffusion -------------------------------------------------------------

def harmonize_video(frames: list[Image.Image], seed: int, strength: float) -> np.ndarray:
    """SDEdit as in tiles.harmonize, but starting the schedule at the
    strength's step instead of computing and discarding the earlier ones."""
    import torch
    from diffusers import FlowMatchEulerDiscreteScheduler

    class Truncated(FlowMatchEulerDiscreteScheduler):
        start = 0

        def set_timesteps(self, *a, **kw):
            super().set_timesteps(*a, **kw)
            self.timesteps = self.timesteps[self.start:]
            self.sigmas = self.sigmas[self.start:]

    p = pipe()
    sched = Truncated(shift=3.0)
    # Same start as tiles.harmonize: its last forced step lands on sigma[lock - 1].
    sched.start = max(0, round((1 - strength) * STEPS) - 1)
    probe = FlowMatchEulerDiscreteScheduler(shift=3.0)
    probe.set_timesteps(STEPS)
    sigma = probe.sigmas[sched.start].item()
    p.scheduler = sched

    W = frames[0].size[0]
    gen = torch.Generator("cpu").manual_seed(seed)
    noise = torch.randn((1, 16, (FRAMES - 1) // 4 + 1, W // VAE, W // VAE), generator=gen)
    x0 = encode_video(p, frames)
    out = p(
        video=frames,
        mask=[Image.new("L", frames[0].size, 255)] * FRAMES,
        conditioning_scale=0.0,
        prompt_embeds=embed(CLIP_PROMPT).to("cuda"),
        negative_prompt_embeds=embed(NEGATIVE).to("cuda"),
        height=W,
        width=W,
        num_frames=FRAMES,
        num_inference_steps=STEPS,
        guidance_scale=5.0,
        latents=(1 - sigma) * x0 + sigma * noise,
        generator=gen,
        output_type="np",
    ).frames[0]
    return (np.clip(out, 0, 1) * 255).round().astype(np.uint8)


def encode_video(p, frames: list[Image.Image]):
    import torch

    v = np.stack([np.asarray(f, np.float32) for f in frames]) / 127.5 - 1  # (F, H, W, 3)
    v = torch.from_numpy(v).permute(3, 0, 1, 2)[None]
    vae = p.vae
    with torch.no_grad():
        z = vae.encode(v.to("cuda", vae.dtype)).latent_dist.mode().float()
    mean = torch.tensor(vae.config.latents_mean).view(1, -1, 1, 1, 1).to(z)
    std = torch.tensor(vae.config.latents_std).view(1, -1, 1, 1, 1).to(z)
    return ((z - mean) / std).cpu()


# --- Stitch, pin, pack -----------------------------------------------------

def stitch_all(worlds: list[np.ndarray], raws: list[np.ndarray]) -> list[np.ndarray]:
    """tiles.stitch over several videos at once: one leader clip per key,
    the medoid over all occurrences in all worlds."""
    pad = B + 2 * FEATHER
    srcs = [np.pad(r, ((0, 0), (pad, pad), (pad, pad), (0, 0)), mode="edge") for r in raws]
    ps = [[(key, (y0 + pad, y1 + pad, x0 + pad, x1 + pad), w) for key, (y0, y1, x0, x1), w in pieces(g)]
          for g in worlds]
    occ = defaultdict(list)
    for wi, wps in enumerate(ps):
        for key, (y0, y1, x0, x1), _ in wps:
            occ[key].append((wi, y0, y1, x0, x1))
    leaders = {}
    for key, os_ in occ.items():
        crop = lambda o: srcs[o[0]][:, o[1]:o[2], o[3]:o[4]].astype(np.float32)
        mean = sum(crop(o) for o in os_) / len(os_)
        best = min(os_, key=lambda o: ((crop(o) - mean) ** 2).sum())
        leaders[key] = crop(best)
    out = []
    for wi, src in enumerate(srcs):
        dst = src.astype(np.float32)
        for key, (y0, y1, x0, x1), w in ps[wi]:
            w = w[None, ..., None]
            dst[:, y0:y1, x0:x1] = dst[:, y0:y1, x0:x1] * (1 - w) + leaders[key] * w
        out.append(dst[:, pad:-pad, pad:-pad])
    return out


def still_world(states: np.ndarray, stills: np.ndarray) -> np.ndarray:
    """Assemble M2 tiles over the interior vertices of a world. The outer
    half-cell ring is never cropped into a tile, so it's left as NaN."""
    W = N * C
    im = np.full((W, W, 3), np.nan, np.float32)
    for vy in range(1, N):
        for vx in range(1, N):
            i = config(states, vx, vy, 3)
            im[vy * C - C // 2:vy * C + C // 2, vx * C - C // 2:vx * C + C // 2] = stills[i]
    return im


def pin(world: np.ndarray, g: np.ndarray, stills: np.ndarray) -> np.ndarray:
    """Blend the first and last PIN frames into the exact stills."""
    ends = [(still_world(PREV[g], stills), lambda f: f), (still_world(CUR[g], stills), lambda f: FRAMES - 1 - f)]
    out = world.copy()
    for still, dist in ends:
        ok = ~np.isnan(still)
        for f in range(FRAMES):
            w = max(0.0, 1 - dist(f) / PIN)
            if w > 0:
                out[f][ok] = out[f][ok] * (1 - w) + still[ok] * w
    return out


def load_stills() -> np.ndarray:
    sheet = np.asarray(Image.open(OUT / "tiles.png").convert("RGB"), np.float32)
    assert json.loads((OUT / "tiles.json").read_text())["tile"] == C, "stills must be generated at the clip tile size"
    return np.stack([sheet[(i // 9) * C:(i // 9 + 1) * C, (i % 9) * C:(i % 9 + 1) * C] for i in range(81)])


def cell_outliers(g: np.ndarray, raw: np.ndarray, means: dict[int, np.ndarray]) -> np.ndarray:
    cells = raw.astype(np.float32).reshape(FRAMES, N, C, N, C, 3).transpose(1, 3, 0, 2, 4, 5)
    return np.array([[((cells[y, x] - means[g[y, x]]) ** 2).mean() for x in range(N)] for y in range(N)])


def pack(worlds: list[np.ndarray], raws: list[np.ndarray]):
    stills = load_stills()
    final = [pin(w, g, stills) for w, g in zip(stitch_all(worlds, raws), worlds)]
    # Per transition, the mean cell clip; a tile is taken from the occurrence whose cells are most typical.
    sums = defaultdict(lambda: 0.0)
    counts = defaultdict(int)
    for g, raw in zip(worlds, raws):
        cells = raw.astype(np.float32).reshape(FRAMES, N, C, N, C, 3).transpose(1, 3, 0, 2, 4, 5)
        for y in range(N):
            for x in range(N):
                sums[g[y, x]] = sums[g[y, x]] + cells[y, x]
                counts[g[y, x]] += 1
    means = {k: sums[k] / counts[k] for k in sums}
    best = {}
    for wi, (g, raw) in enumerate(zip(worlds, raws)):
        bad = cell_outliers(g, raw, means)
        for vy in range(1, N):
            for vx in range(1, N):
                score = bad[vy - 1:vy + 1, vx - 1:vx + 1].sum()
                i = config(g, vx, vy)
                if i not in best or score < best[i][0]:
                    best[i] = (score, wi, vx, vy)
    assert len(best) == CONFIGS, f"worlds cover only {len(best)} configs"

    S = OUT_TILE + 2 * GUTTER
    side = round(CONFIGS ** 0.5)
    for f in range(FRAMES):
        page = np.zeros((side * S, side * S, 3), np.uint8)
        for i, (_, wi, vx, vy) in best.items():
            t = final[wi][f, vy * C - C // 2:vy * C + C // 2, vx * C - C // 2:vx * C + C // 2]
            t = np.asarray(Image.fromarray(t.round().clip(0, 255).astype(np.uint8)).resize((OUT_TILE, OUT_TILE), Image.LANCZOS))
            t = np.pad(t, ((GUTTER, GUTTER), (GUTTER, GUTTER), (0, 0)), mode="edge")
            page[(i // side) * S:(i // side + 1) * S, (i % side) * S:(i % side + 1) * S] = t
        Image.fromarray(page).save(OUT / f"clips-{f:02d}.png")
    meta = {"version": 1, "tile": OUT_TILE, "gutter": GUTTER, "frames": FRAMES, "columns": side,
            "transitions": list(TRANSITIONS)}
    (OUT / "clips.json").write_text(json.dumps(meta) + "\n")
    log.info("wrote %d pages to %s", FRAMES, OUT)
    for wi, w in enumerate(final):
        sheet = np.concatenate([w[f] for f in range(0, FRAMES, 4)], 1)
        Image.fromarray(sheet.round().clip(0, 255).astype(np.uint8)).resize((sheet.shape[1] // 2, sheet.shape[0] // 2)).save(WORK / f"final_{wi:02d}.jpg")


# --- Main ------------------------------------------------------------------

def load_worlds() -> list[np.ndarray]:
    return [np.array(w) for w in json.loads((WORK / "worlds.json").read_text())]


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("stage", choices=["worlds", "gen", "pack", "all"], default="all", nargs="?")
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--strength", type=float, default=0.6)
    ap.add_argument("--probe", action="store_true", help="gen: only world 0, and save a contact sheet")
    args = ap.parse_args()
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(name)s %(message)s")
    logging.getLogger("httpx").setLevel(logging.WARNING)
    WORK.mkdir(parents=True, exist_ok=True)

    if args.stage in ("worlds", "all") or not (WORK / "worlds.json").exists():
        worlds = make_worlds()
        (WORK / "worlds.json").write_text(json.dumps([w.tolist() for w in worlds]) + "\n")
    worlds = load_worlds()

    if args.stage in ("gen", "all"):
        sp = Sprites()
        todo = [0] if args.probe else range(len(worlds))
        for wi in todo:
            path = WORK / f"raw_{wi:02d}.npy"
            if path.exists() and not args.probe:
                continue
            frames = collage_video(worlds[wi], sp)
            lease = turnstile.acquire(est_seconds=600)
            try:
                log.info("world %d/%d: harmonize", wi, len(worlds))
                raw = harmonize_video(frames, args.seed + wi, args.strength)
            finally:
                if lease:
                    lease.release()
            np.save(path, raw)
            if args.probe:
                col = np.concatenate([np.asarray(frames[f]) for f in range(0, FRAMES, 4)], 1)
                out = np.concatenate([raw[f] for f in range(0, FRAMES, 4)], 1)
                Image.fromarray(np.concatenate([col, out], 0)).save(WORK / "probe.jpg")

    if args.stage in ("pack", "all"):
        raws = [np.load(WORK / f"raw_{wi:02d}.npy") for wi in range(len(worlds))]
        pack(worlds, raws)


if __name__ == "__main__":
    main()
