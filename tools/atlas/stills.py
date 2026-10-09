"""Generate the forest stills atlas (docs/PLAN.md, M4).

The M2 recipe (collage -> harmonize -> stitch, see tiles.py) with five looks
instead of three, so a vertex tile is one of 5^4 = 625 configurations:

  grass, pine forest, burning forest, burnt stand, bare dirt

Sim states map onto looks in the renderer (src/shaders.ts). Regrowth from
dirt to grass is a blend between two looks, drawn by the shader.

1. Keys: one texture per look. Forest, grass and dirt are text-to-image;
   burning and burnt are SDEdits of the forest, so the same trees burn.
   Each is cropped and made periodic with period C, so a run of cells of one
   look is seamless in the collage.
2. Worlds: a few 12x12 grids that together contain every 2x2 configuration
   (one world would be too big for one pass).
3. Gen: per world, a collage of the keys (ground looks first, then canopy
   looks overhanging their cells with ragged edges) harmonized by one SDEdit
   pass. Raw worlds are saved, so the stage resumes after an interruption.
4. Pack: stitch every world with one leader per border piece over all
   worlds, and crop each configuration into a 25x25 atlas.

Usage: python stills.py [keys|worlds|gen|pack|all] [--probe] [--force]
"""

from __future__ import annotations

import argparse
import json
import logging
import random
from collections import defaultdict

import numpy as np
from PIL import Image

import turnstile
from generate import DATA
from tiles import B, C, F, harmonize, pieces, typical

WORK = DATA / "work" / "stills"
OUT = DATA / "atlas"

N = 12  # world size in cells (1152 px, the largest that fits the 3080)
LOOKS = ["grass", "forest", "fire", "burnt", "dirt"]
GRASS, FOREST, FIRE, BURNT, DIRT = range(5)
K = len(LOOKS)
CONFIGS = K ** 4
KEY_SIZE = 512
# Drawn in this order; the canopy looks overhang neighboring cells by
# MARGIN of a cell, with a ragged edge.
LAYERS = [GRASS, DIRT, BURNT, FOREST, FIRE]
MARGIN = {GRASS: 0.0, DIRT: 0.08, BURNT: 0.12, FOREST: 0.14, FIRE: 0.14}
# Crop from the 512 px keys before scaling to one cell, which sets how big
# the trees come out.
SCALE = 192

STYLE = ("seen from directly above, top-down orthographic aerial view, painterly stylized game art, "
         "soft even lighting, detailed texture filling the whole frame")
KEYS = {
    "forest": ("dense uniform pine forest canopy: dark green conifer crowns packed tightly together, pointed spruce "
               "tops casting small shadows, no clearings, no ground visible", None, None),
    "grass": ("aerial drone photo looking straight down at a lush green lawn: short dense vibrant grass seen from "
              "high above, soft mottled texture, no trees, no flowers, no paths", None, None),
    "dirt": ("bare scorched dark brown earth covered in grey ash and black char, freshly burnt ground, no plants, "
             "no trees", None, None),
    # SDEdits of the forest key, prepared in pixel space first (the model
    # keeps a green canopy green): (prompt, strength, prep).
    "fire": ("a pine forest canopy engulfed in a raging crown fire: bright orange and yellow flames covering most of "
             "the conifer crowns, glowing embers, wisps of smoke", 0.7, "ignite"),
    "burnt": ("the black charred crowns of dead burnt pine trees seen from directly above: bare blackened star-shaped "
              "branches, grey ash and scorched brown earth between them, smouldering, no green", 0.6, "char"),
}
PROMPT = ("top-down orthographic aerial view straight down at a landscape: dense dark green pine forest, lush green "
          "grass meadow, a wildfire with bright orange flames in the forest, burnt-out forest of black charred snags, "
          "bare scorched brown earth with grey ash, painterly stylized game art, soft even lighting, detailed texture")

log = logging.getLogger("stills")


# --- Keys ------------------------------------------------------------------

def make_keys(seed: int, force: bool):
    d = WORK / "keys"
    d.mkdir(parents=True, exist_ok=True)
    blank = Image.new("RGB", (KEY_SIZE, KEY_SIZE), (128, 128, 128))
    for i, (name, (prompt, strength, prep)) in enumerate(KEYS.items()):
        path = d / f"{name}.png"
        if path.exists() and not force:
            continue
        log.info("key: %s", name)
        src = prepare(Image.open(d / "forest.png").convert("RGB"), prep) if prep else blank
        harmonize(src, seed + i, strength or 1.0, f"{prompt}, {STYLE}").save(path)


def prepare(forest: Image.Image, prep: str) -> Image.Image:
    """Push the forest toward a look before SDEdit, keeping its trees."""
    a = np.asarray(forest, np.float32)
    lum = a @ np.array([0.299, 0.587, 0.114], np.float32)
    if prep == "char":
        # Crowns go to black wood, the dark gaps between them to ash.
        wood = (12 + 0.3 * lum[..., None]) * np.array([1.0, 0.9, 0.82])
        ash = np.array([88, 76, 64], np.float32)
        w = np.clip((lum - 30) / 40, 0, 1)[..., None]
        out = ash * (1 - w) + wood * w
    else:
        # Flames in blobs over most of the canopy.
        blob = (ragged(KEY_SIZE, 7) + 1) / 2
        w = np.clip((blob - 0.35) * 4, 0, 1)[..., None]
        flame = np.array([255, 140, 30], np.float32) * (0.25 + 1.1 * lum[..., None] / 255)
        out = a * (1 - 0.85 * w) + flame * 0.85 * w
    return Image.fromarray(out.clip(0, 255).astype(np.uint8))


def periodic(tex: np.ndarray) -> np.ndarray:
    """A C x C tile that wraps seamlessly: the crop, cross-faded near its
    edges into the same crop shifted by half a period."""
    t = tex[:C, :C]
    shifted = np.roll(tex[:C, :C], (C // 2, C // 2), (0, 1))
    # Weight of the original: 1 in the middle, 0 at the edges.
    r = np.minimum(np.arange(C), C - 1 - np.arange(C)) + 0.5
    e = np.clip(r / (C * 0.25), 0, 1)
    w = np.minimum(e[:, None], e[None, :])[..., None]
    w = w * w * (3 - 2 * w)
    return t * w + shifted * (1 - w)


def load_keys() -> dict[int, np.ndarray]:
    out = {}
    for look, name in enumerate(LOOKS):
        im = Image.open(WORK / "keys" / f"{name}.png").convert("RGB")
        o = (KEY_SIZE - SCALE) // 2
        im = im.crop((o, o, o + SCALE, o + SCALE)).resize((C, C), Image.LANCZOS)
        out[look] = periodic(np.asarray(im, np.float32))
    return out


# --- Worlds ----------------------------------------------------------------

def config(g: np.ndarray, vx: int, vy: int) -> int:
    """Config index of the tile at vertex (vx, vy), image coords (y down)."""
    nw, ne, sw, se = g[vy - 1, vx - 1], g[vy - 1, vx], g[vy, vx - 1], g[vy, vx]
    return int(sw + K * se + K * K * nw + K ** 3 * ne)


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

def ragged(size: int, seed: int) -> np.ndarray:
    """Smooth value noise in [-1, 1] on a size x size grid."""
    rnd = np.random.default_rng(seed)
    out = np.zeros((size, size), np.float32)
    for cells, amp in [(4, 0.6), (8, 0.3), (16, 0.1)]:
        lo = rnd.uniform(-1, 1, (cells + 1, cells + 1)).astype(np.float32)
        out += amp * np.asarray(Image.fromarray(lo).resize((size, size), Image.BICUBIC))
    return out


def collage(g: np.ndarray, keys: dict[int, np.ndarray]) -> Image.Image:
    """Each cell shows its look's periodic texture (aligned to the world, so
    runs of one look are seamless) inside a mask that's the same for every
    cell of that look: the cell square grown by the look's margin, with a
    ragged edge. Looks are drawn in layer order, so overlaps resolve the same
    way everywhere."""
    W = N * C
    reps = N + 2
    tex = {k: np.tile(t, (reps, reps, 1)) for k, t in keys.items()}  # world shifted by one cell
    im = tex[GRASS][C:C + W, C:C + W].copy()
    S = 2 * C  # mask window, centered on the cell
    u = (np.arange(S) + 0.5) / C - 1.0  # cell-local, cell spans [-0.5, 0.5]
    box = np.maximum(np.abs(u)[:, None], np.abs(u)[None, :])
    masks = {}
    for look in LAYERS:
        edge = 0.5 + MARGIN[look] * (1 + ragged(S, 100 + look))
        masks[look] = np.clip((edge - box) * C / 3 + 0.5, 0, 1)[..., None]
    pad = np.pad(im, ((C, C), (C, C), (0, 0)))
    for look in LAYERS[1:]:
        for y in range(N):
            for x in range(N):
                if g[y, x] != look:
                    continue
                y0, x0 = y * C + C // 2, x * C + C // 2  # window origin in padded coords
                src = tex[look][y0:y0 + S, x0:x0 + S]
                m = masks[look]
                pad[y0:y0 + S, x0:x0 + S] = pad[y0:y0 + S, x0:x0 + S] * (1 - m) + src * m
    return Image.fromarray(pad[C:C + W, C:C + W].round().clip(0, 255).astype(np.uint8))


# --- Stitch and pack -------------------------------------------------------

def stitch_all(worlds: list[np.ndarray], raws: list[np.ndarray]) -> list[np.ndarray]:
    """tiles.stitch over several worlds at once: one leader per key, the
    occurrence closest to the per-pixel median over all worlds."""
    pad = B + 2 * F
    srcs = [np.pad(r.astype(np.float32), ((pad, pad), (pad, pad), (0, 0)), mode="edge") for r in raws]
    ps = [[(key, (y0 + pad, y1 + pad, x0 + pad, x1 + pad), w) for key, (y0, y1, x0, x1), w in pieces(g)]
          for g in worlds]
    crops = defaultdict(list)
    for wi, wps in enumerate(ps):
        for key, (y0, y1, x0, x1), _ in wps:
            crops[key].append(srcs[wi][y0:y1, x0:x1])
    leaders = {key: cs[typical(np.stack(cs))] for key, cs in crops.items()}
    out = []
    for wi, src in enumerate(srcs):
        dst = src.copy()
        for key, (y0, y1, x0, x1), w in ps[wi]:
            w = w[..., None]
            dst[y0:y1, x0:x1] = dst[y0:y1, x0:x1] * (1 - w) + leaders[key] * w
        out.append(dst[pad:-pad, pad:-pad])
    return out


def pack(worlds: list[np.ndarray], raws: list[np.ndarray]):
    final = stitch_all(worlds, raws)
    # Per look, the mean raw cell; each config is cropped from the occurrence
    # whose cells are most typical.
    cells = [r.astype(np.float32).reshape(N, C, N, C, 3).transpose(0, 2, 1, 3, 4) for r in raws]
    means = {k: np.mean([c[g == k].mean(0) for g, c in zip(worlds, cells) if (g == k).any()], 0) for k in range(K)}
    best = {}
    for wi, (g, c) in enumerate(zip(worlds, cells)):
        bad = np.array([[((c[y, x] - means[g[y, x]]) ** 2).mean() for x in range(N)] for y in range(N)])
        for vy in range(1, N):
            for vx in range(1, N):
                score = bad[vy - 1:vy + 1, vx - 1:vx + 1].sum()
                i = config(g, vx, vy)
                if i not in best or score < best[i][0]:
                    best[i] = (score, wi, vx, vy)
    assert len(best) == CONFIGS, f"worlds cover only {len(best)} configs"
    side = K * K
    sheet = np.zeros((side * C, side * C, 3), np.float32)
    for i, (_, wi, vx, vy) in best.items():
        sheet[(i // side) * C:(i // side + 1) * C, (i % side) * C:(i % side + 1) * C] = \
            final[wi][vy * C - C // 2:vy * C + C // 2, vx * C - C // 2:vx * C + C // 2]
    OUT.mkdir(parents=True, exist_ok=True)
    Image.fromarray(sheet.round().clip(0, 255).astype(np.uint8)).save(OUT / "stills.png")
    (OUT / "stills.json").write_text(json.dumps({"version": 1, "tile": C, "looks": LOOKS}) + "\n")
    log.info("wrote %s", OUT / "stills.png")
    for wi, w in enumerate(final):
        Image.fromarray(w.round().clip(0, 255).astype(np.uint8)).save(WORK / f"world_{wi:02d}.jpg", quality=90)


# --- Main ------------------------------------------------------------------

def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("stage", choices=["keys", "worlds", "gen", "pack", "all"], default="all", nargs="?")
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--strength", type=float, default=0.6)
    ap.add_argument("--force", action="store_true", help="keys/gen: regenerate outputs that already exist")
    ap.add_argument("--probe", action="store_true", help="gen: only world 0, saved as probe.jpg next to its collage")
    args = ap.parse_args()
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(name)s %(message)s")
    logging.getLogger("httpx").setLevel(logging.WARNING)
    WORK.mkdir(parents=True, exist_ok=True)

    if args.stage in ("keys", "all"):
        lease = turnstile.acquire(est_seconds=300)
        try:
            make_keys(args.seed, args.force)
        finally:
            if lease:
                lease.release()

    if args.stage in ("worlds", "all") or not (WORK / "worlds.json").exists():
        (WORK / "worlds.json").write_text(json.dumps([w.tolist() for w in make_worlds()]) + "\n")
    worlds = [np.array(w) for w in json.loads((WORK / "worlds.json").read_text())]

    if args.stage in ("gen", "all"):
        keys = load_keys()
        todo = [0] if args.probe else range(len(worlds))
        for wi in todo:
            path = WORK / f"raw_{wi:02d}.png"
            if path.exists() and not (args.force or args.probe):
                continue
            lay = collage(worlds[wi], keys)
            lay.save(WORK / f"collage_{wi:02d}.png")
            lease = turnstile.acquire(est_seconds=180)
            try:
                log.info("world %d/%d: harmonize", wi, len(worlds))
                raw = harmonize(lay, args.seed + wi, args.strength, PROMPT)
            finally:
                if lease:
                    lease.release()
            raw.save(path)
            if args.probe:
                Image.fromarray(np.concatenate([np.asarray(lay), np.asarray(raw)], 1)).save(WORK / "probe.jpg", quality=90)

    if args.stage in ("pack", "all"):
        raws = [np.asarray(Image.open(WORK / f"raw_{wi:02d}.png").convert("RGB")) for wi in range(len(worlds))]
        pack(worlds, raws)


if __name__ == "__main__":
    main()
