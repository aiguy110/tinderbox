"""Generate the dual-grid stills atlas (docs/PLAN.md, M2).

Every tile border runs through cell centers, so a border is seamless if its
pixels depend only on the two cells it joins (and each tile corner only on
its one cell). Pipeline:

1. Lay out a small world that contains every 2x2 configuration, as a sharp
   collage of the v0 keyframes (ground texture, tree and burning-tree discs).
2. Harmonize it with one SDEdit pass of Wan VACE (re-noise to `strength`,
   denoise). Because the collage puts identical pixels at every cell of a
   state, all occurrences of a border piece come out nearly identical.
3. Stitch: for each border piece, paste one leader occurrence over all the
   others with feathered weights. Pieces are keyed by what they may depend
   on: corner (state) around each cell center, h strip (west, east) and
   v strip (south, north) along the lines between centers. Corners get full
   weight wherever strips overlap, so the border lines are exact.
4. Crop one tile per configuration into a 9x9 atlas.

Synchronizing latents during denoising (averaging or copying the pieces
after every step) was tried first. It wrecks the image; see docs/PLAN.md.

Usage: python tiles.py [--seed N] [--strength S]
"""

from __future__ import annotations

import argparse
import json
import logging
import random
from collections import defaultdict

import numpy as np
from PIL import Image, ImageDraw, ImageFilter

import turnstile
from generate import DATA, NEGATIVE, embed, pipe

WORK = DATA / "work" / "tiles"
OUT = DATA / "atlas"

C = 96  # cell (= tile) size in px; a multiple of 16 (VAE 8 x patch 2)
N = 12  # world size in cells
B = 8  # stitched pieces have full weight within B px of their line
F = 16  # ...and feather out over the next F px
VAE = 8
STEPS = 40
EMPTY, TREE, FIRE = 0, 1, 2

PROMPT = (
    "top-down orthographic aerial view straight down at a forest clearing: round leafy green tree "
    "canopies, some trees burning with bright orange flames, glowing embers and smoke, and patches of bare "
    "scorched brown earth with grey ash, painterly stylized game art, soft even lighting, detailed texture"
)

log = logging.getLogger("tiles")


# --- World layout ----------------------------------------------------------

def world_states(seed: int = 0) -> np.ndarray:
    """An N x N grid (row 0 = north) in which every 2x2 window of the 3
    states appears at least once. Found by annealing; N=12 converges fast."""
    rnd = random.Random(seed)
    g = np.array([[rnd.randrange(3) for _ in range(N)] for _ in range(N)])

    def missing() -> int:
        return 81 - len({tuple(g[y:y + 2, x:x + 2].ravel()) for y in range(N - 1) for x in range(N - 1)})

    m, temp = missing(), 2.0
    while m:
        x, y = rnd.randrange(N), rnd.randrange(N)
        old = g[y, x]
        g[y, x] = rnd.randrange(3)
        m2 = missing()
        if m2 <= m or rnd.random() < np.exp((m - m2) / temp):
            m = m2
        else:
            g[y, x] = old
        temp = max(0.02, temp * 0.99997)
    return g


def config(g: np.ndarray, vx: int, vy: int) -> int:
    """Config index of the tile at vertex (vx, vy), image coords (y down)."""
    nw, ne, sw, se = g[vy - 1, vx - 1], g[vy - 1, vx], g[vy, vx - 1], g[vy, vx]
    return int(sw + 3 * se + 9 * nw + 27 * ne)


def collage(g: np.ndarray) -> Image.Image:
    """Sharp layout made from the v0 keyframes: ground texture everywhere,
    plus a tree or burning-tree disc on each TREE/FIRE cell. (A blurry
    color-blob layout makes the model paint a blurry world.)"""
    kf = DATA / "work" / "keyframes"
    W = N * C
    ground = Image.open(kf / "empty.png").convert("RGB").resize((2 * C, 2 * C), Image.LANCZOS)
    im = Image.new("RGB", (W, W))
    for y in range(0, W, 2 * C):
        for x in range(0, W, 2 * C):
            im.paste(ground, (x, y))
    # The keyframe canopies span roughly the middle 75% of the frame.
    d = int(C / 0.75 * 0.9)
    disc = Image.new("L", (d, d))
    ImageDraw.Draw(disc).ellipse([d * 0.12, d * 0.12, d * 0.88, d * 0.88], fill=255)
    disc = disc.filter(ImageFilter.GaussianBlur(d // 30))
    sprites = {s: Image.open(kf / f).convert("RGB").resize((d, d), Image.LANCZOS)
               for s, f in [(TREE, "tree.png"), (FIRE, "fire.png")]}
    for y in range(N):
        for x in range(N):
            if g[y, x] in sprites:
                im.paste(sprites[g[y, x]], (x * C + C // 2 - d // 2, y * C + C // 2 - d // 2), disc)
    return im


# --- Diffusion -------------------------------------------------------------

def encode(p, im: Image.Image):
    """VAE-encode one image into normalized latents (1, 16, 1, h, w)."""
    import torch

    v = torch.from_numpy(np.asarray(im, np.float32) / 127.5 - 1).permute(2, 0, 1)[None, :, None]
    vae = p.vae
    with torch.no_grad():
        z = vae.encode(v.to("cuda", vae.dtype)).latent_dist.mode().float()
    mean = torch.tensor(vae.config.latents_mean).view(1, -1, 1, 1, 1).to(z)
    std = torch.tensor(vae.config.latents_std).view(1, -1, 1, 1, 1).to(z)
    return ((z - mean) / std).cpu()


def harmonize(im: Image.Image, seed: int, strength: float, prompt: str = PROMPT) -> Image.Image:
    """SDEdit: hold the latents on the noised input's trajectory for the
    first (1 - strength) of the steps, then denoise freely. Strength 1 is
    plain text-to-image."""
    import torch
    from diffusers import FlowMatchEulerDiscreteScheduler

    p = pipe()
    # Single-step solver: UniPC's corrector keeps its own copy of the
    # previous sample, which fights the latents on_step writes.
    p.scheduler = FlowMatchEulerDiscreteScheduler(shift=3.0)
    W = im.size[0]
    gen = torch.Generator("cpu").manual_seed(seed)
    noise = torch.randn((1, 16, 1, W // VAE, W // VAE), generator=gen)
    x0 = encode(p, im)
    lock = round((1 - strength) * STEPS)

    def on_step(_pipe, i, t, kw):
        lat = kw["latents"]
        if i + 1 < lock:
            s = _pipe.scheduler.sigmas[i + 1].item()
            lat = (1 - s) * x0.to(lat) + s * noise.to(lat)
        return {"latents": lat}

    out = p(
        video=[im],
        mask=[Image.new("L", im.size, 255)],
        conditioning_scale=0.0,  # VACE control copies the input too literally
        prompt_embeds=embed(prompt).to("cuda"),
        negative_prompt_embeds=embed(NEGATIVE).to("cuda"),
        height=W,
        width=W,
        num_frames=1,
        num_inference_steps=STEPS,
        guidance_scale=5.0,
        latents=noise.clone(),
        generator=gen,
        callback_on_step_end=on_step,
        output_type="np",
    ).frames[0][0]
    return Image.fromarray((np.clip(out, 0, 1) * 255).round().astype(np.uint8))


# --- Stitching -------------------------------------------------------------

def ramp(d: np.ndarray, full: float) -> np.ndarray:
    """1 within `full` px, falling linearly to 0 over the next F px."""
    return np.clip((full + F - d) / F, 0, 1)


def pieces(g: np.ndarray):
    """(key, box (y0, y1, x0, x1), weight) for every border piece, strips
    first so corners are composited over them."""
    ctr = lambda i: i * C + C // 2
    r = B + F  # strip half-width
    a = np.abs(np.arange(-r, r) + 0.5)
    strip_w = ramp(a, B)  # by perpendicular distance
    h_w = np.repeat(strip_w[:, None], C, 1)
    v_w = h_w.T
    rc = B + 2 * F  # corner half-size: full weight out to B+F, so strips never touch the lines there
    ac = np.abs(np.arange(-rc, rc) + 0.5)
    corner_w = ramp(np.maximum(ac[:, None], ac[None, :]), B + F)
    out = []
    n = len(g)
    for y in range(n):
        for x in range(n):
            cx, cy = ctr(x), ctr(y)
            if x + 1 < n:
                out.append((("h", g[y, x], g[y, x + 1]), (cy - r, cy + r, cx, cx + C), h_w))
            if y + 1 < n:
                out.append((("v", g[y + 1, x], g[y, x]), (cy, cy + C, cx - r, cx + r), v_w))
    for y in range(n):
        for x in range(n):
            cx, cy = ctr(x), ctr(y)
            out.append((("c", g[y, x]), (cy - rc, cy + rc, cx - rc, cx + rc), corner_w))
    return out


def typical(stack: np.ndarray) -> int:
    """Index of the occurrence closest to the per-pixel median. Stray flecks
    land in a different place in each occurrence, so the median drops them;
    the mean keeps a faint copy of each, and the medoid by mean still picked
    a ground corner with a green streak in it, repeated over every tile."""
    return int(np.abs(stack - np.median(stack, 0)).sum(tuple(range(1, stack.ndim))).argmin())


def stitch(g: np.ndarray, im: Image.Image) -> Image.Image:
    src = np.asarray(im, np.float32)
    W = src.shape[0]
    pad = B + 2 * F
    src = np.pad(src, ((pad, pad), (pad, pad), (0, 0)), mode="edge")
    dst = src.copy()
    ps = [(key, (y0 + pad, y1 + pad, x0 + pad, x1 + pad), w) for key, (y0, y1, x0, x1), w in pieces(g)]
    crops = defaultdict(list)
    for key, (y0, y1, x0, x1), _ in ps:
        crops[key].append(src[y0:y1, x0:x1])
    leaders = {key: cs[typical(np.stack(cs))] for key, cs in crops.items()}
    for key, (y0, y1, x0, x1), w in ps:
        w = w[..., None]
        dst[y0:y1, x0:x1] = dst[y0:y1, x0:x1] * (1 - w) + leaders[key] * w
    out = dst[pad:pad + W, pad:pad + W]
    return Image.fromarray(out.round().clip(0, 255).astype(np.uint8))


def cell_outliers(g: np.ndarray, raw: Image.Image) -> np.ndarray:
    """Per cell, the squared distance of its raw pixels from the mean of all
    cells in the same state. High means the model drew something atypical."""
    a = np.asarray(raw, np.float32)
    cells = a.reshape(N, C, N, C, 3).transpose(0, 2, 1, 3, 4)
    out = np.zeros((N, N))
    for s in range(3):
        m = g == s
        out[m] = ((cells[m] - cells[m].mean(0)) ** 2).mean((1, 2, 3))
    return out


def write_atlas(g: np.ndarray, world: Image.Image, raw: Image.Image):
    """Crop each configuration from its most typical occurrence."""
    bad = cell_outliers(g, raw)
    best = {}
    for vy in range(1, N):
        for vx in range(1, N):
            score = bad[vy - 1:vy + 1, vx - 1:vx + 1].sum()
            i = config(g, vx, vy)
            if i not in best or score < best[i][0]:
                best[i] = (score, vx, vy)
    sheet = Image.new("RGB", (9 * C, 9 * C))
    for i, (_, vx, vy) in best.items():
        x0, y0 = vx * C - C // 2, vy * C - C // 2
        sheet.paste(world.crop((x0, y0, x0 + C, y0 + C)), ((i % 9) * C, (i // 9) * C))
    seen = best
    assert len(seen) == 81, f"world covers only {len(seen)} configs"
    OUT.mkdir(parents=True, exist_ok=True)
    sheet.save(OUT / "tiles.png")
    (OUT / "tiles.json").write_text(json.dumps({"version": 1, "tile": C}) + "\n")
    log.info("wrote %s", OUT / "tiles.png")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--strength", type=float, default=0.6, help="SDEdit strength (1 = ignore the collage)")
    ap.add_argument("--restitch", action="store_true", help="skip diffusion; re-stitch the saved raw world")
    args = ap.parse_args()
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(name)s %(message)s")
    logging.getLogger("httpx").setLevel(logging.WARNING)

    WORK.mkdir(parents=True, exist_ok=True)
    g = world_states()
    if args.restitch:
        raw = Image.open(WORK / "world_raw.png").convert("RGB")
    else:
        lay = collage(g)
        lay.save(WORK / "collage.png")
        lease = turnstile.acquire(est_seconds=180)
        try:
            log.info("harmonize (strength=%s, seed=%d)", args.strength, args.seed)
            raw = harmonize(lay, args.seed, args.strength)
        finally:
            if lease:
                lease.release()
        raw.save(WORK / "world_raw.png")
    world = stitch(g, raw)
    world.save(WORK / "world.png")
    write_atlas(g, world, raw)


if __name__ == "__main__":
    main()
