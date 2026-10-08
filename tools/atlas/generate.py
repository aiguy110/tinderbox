"""Generate tinderbox's sprite atlas with a video diffusion model.

Every clip is keyframe-interpolated between three shared state stills
(empty ground, tree, burning tree), so the last frame of one transition is
exactly the first frame of the next state's clip and cells never pop:

    keyframes  tree.png from text; fire.png and empty.png from a burn clip
               seeded on tree.png. Drop your own PNGs in $TINDERBOX_DATA/work/keyframes/ to
               override any of them.
    clips      one first/last-frame-conditioned clip per transition
               (Wan2.1-VACE-1.3B). Spread clips also get a half-burnt
               midpoint frame wiped in from the source side, which pins the
               direction the fire travels.
    pack       sample each clip down to --frames tiles and write
               $TINDERBOX_DATA/atlas/atlas.{png,json}.

Usage: python generate.py [keyframes|clips|pack|all] [--only CLIP ...] [--force]
"""

from __future__ import annotations

import argparse
import functools
import json
import logging
import os
from pathlib import Path

import numpy as np
from PIL import Image, ImageFilter

import turnstile

HERE = Path(__file__).resolve().parent
# Binary outputs live outside the repo (see CLAUDE.md); vite.config.ts serves DATA/atlas.
DATA = Path(os.environ.get("TINDERBOX_DATA", Path.home() / ".local/share/tinderbox"))
WORK = DATA / "work"
KEYFRAMES = WORK / "keyframes"
CLIPS_DIR = WORK / "clips"
OUT = DATA / "atlas"

MODEL = "Wan-AI/Wan2.1-VACE-1.3B-diffusers"
SIZE = 512
NUM_FRAMES = 33  # Wan wants 4k + 1
STEPS = 30

STYLE = (
    "top-down orthographic view looking straight down, single square game tile, "
    "painterly stylized forest game art, soft even lighting, centered, fixed camera, no camera motion"
)
NEGATIVE = (
    "camera movement, zoom, pan, perspective, horizon, sky, text, watermark, border, frame, "
    "blurry, low quality, people, animals"
)
KEY_PROMPTS = {
    "tree": "a single round leafy green tree canopy seen from directly above, on dry brown earth",
    "burn": "the tree catches fire, roaring orange flames spread until they engulf the whole canopy",
    "empty": "bare scorched brown earth seen from directly above, scattered grey ash and a few dry grass tufts, "
             "no tree, no plants",
}

# Clips in renderer order (src/shaders.ts CLIPS). `wipe` is the direction the
# fire comes from, for the midpoint anchor.
CLIPS = {
    "empty": dict(start="empty", end="empty", loop=True,
                  prompt="scorched bare brown earth with ash and dry grass, grass stirring in a light breeze"),
    "tree": dict(start="tree", end="tree", loop=True,
                 prompt="a leafy green tree canopy gently swaying in the wind"),
    "grow": dict(start="empty", end="tree",
                 prompt="a sapling sprouts from the bare earth and quickly grows into a full leafy tree"),
    "spread-edge": dict(start="tree", end="fire", wipe="west",
                        prompt="a wall of flames sweeps in from the left edge and spreads across the tree canopy"),
    "spread-corner": dict(start="tree", end="fire", wipe="southwest",
                          prompt="flames creep in from the bottom-left corner and spread across the tree canopy"),
    "lightning": dict(start="tree", end="fire",
                      prompt="a bright white lightning bolt strikes the center of the tree, which bursts into flames"),
    "burnout": dict(start="fire", end="empty",
                    prompt="the burning tree collapses into glowing embers and charred ash on scorched earth"),
}

log = logging.getLogger("atlas")


# --- Diffusion -------------------------------------------------------------

_pipe = None
_encoder = None


def pipe():
    global _pipe
    if _pipe is None:
        import torch
        from diffusers import AutoencoderKLWan, UniPCMultistepScheduler, WanVACEPipeline

        vae = AutoencoderKLWan.from_pretrained(MODEL, subfolder="vae", torch_dtype=torch.float32)
        # The UMT5-XXL text encoder alone is ~11 GB in bf16, more than the
        # 3080 has, so it's left out here and run on the CPU by embed().
        p = WanVACEPipeline.from_pretrained(MODEL, vae=vae, text_encoder=None, torch_dtype=torch.bfloat16)
        p.scheduler = UniPCMultistepScheduler.from_config(p.scheduler.config, flow_shift=3.0)
        p.enable_model_cpu_offload()
        _pipe = p
    return _pipe


@functools.cache
def embed(text: str, max_len: int = 512):
    """UMT5 prompt embedding on the CPU, matching WanVACEPipeline's own."""
    global _encoder
    import torch
    from diffusers.pipelines.wan.pipeline_wan_vace import prompt_clean
    from transformers import UMT5EncoderModel

    if _encoder is None:
        _encoder = UMT5EncoderModel.from_pretrained(MODEL, subfolder="text_encoder", torch_dtype=torch.bfloat16)
    # Encode only the real tokens, then zero-pad as the pipeline does.
    ids = pipe().tokenizer([prompt_clean(text)], max_length=max_len, truncation=True, return_tensors="pt").input_ids
    with torch.no_grad():
        h = _encoder(ids).last_hidden_state
    return torch.cat([h, h.new_zeros(1, max_len - h.shape[1], h.shape[2])], dim=1)


def generate(prompt: str, anchors: dict[int, Image.Image], num_frames: int, seed: int) -> list[Image.Image]:
    """Run VACE with the given frames pinned (index -> image); the rest are
    generated. With no anchors this is plain text-to-video."""
    import torch

    kwargs = {}
    if anchors:
        gray = Image.new("RGB", (SIZE, SIZE), (128, 128, 128))
        keep, fill = Image.new("L", (SIZE, SIZE), 0), Image.new("L", (SIZE, SIZE), 255)
        kwargs["video"] = [anchors[i].resize((SIZE, SIZE)) if i in anchors else gray for i in range(num_frames)]
        kwargs["mask"] = [keep if i in anchors else fill for i in range(num_frames)]
    out = pipe()(
        prompt_embeds=embed(f"{prompt}, {STYLE}").to("cuda"),
        negative_prompt_embeds=embed(NEGATIVE).to("cuda"),
        height=SIZE,
        width=SIZE,
        num_frames=num_frames,
        num_inference_steps=STEPS,
        guidance_scale=5.0,
        generator=torch.Generator("cpu").manual_seed(seed),
        output_type="np",
        **kwargs,
    ).frames[0]
    return [Image.fromarray((np.clip(f, 0, 1) * 255).astype(np.uint8)) for f in out]


def save_clip(name: str, frames: list[Image.Image]):
    import imageio.v3 as iio

    d = CLIPS_DIR / name
    d.mkdir(parents=True, exist_ok=True)
    for old in d.glob("*.png"):
        old.unlink()
    for i, f in enumerate(frames):
        f.save(d / f"{i:03d}.png")
    iio.imwrite(CLIPS_DIR / f"{name}.mp4", np.stack([np.asarray(f) for f in frames]), fps=12)


def wipe(a: Image.Image, b: Image.Image, direction: str) -> Image.Image:
    """Half-way composite: `b` covers the side of `a` the fire comes from."""
    y, x = np.mgrid[0:SIZE, 0:SIZE] / (SIZE - 1)
    along = {"west": x, "southwest": (x + (1 - y)) / 2}[direction]
    mask = Image.fromarray(((along < 0.5) * 255).astype(np.uint8)).filter(ImageFilter.GaussianBlur(SIZE / 24))
    return Image.composite(b.resize((SIZE, SIZE)), a.resize((SIZE, SIZE)), mask)


# --- Stages ----------------------------------------------------------------

def stage_keyframes(seed: int, force: bool):
    KEYFRAMES.mkdir(parents=True, exist_ok=True)
    tree, fire, empty = (KEYFRAMES / f"{k}.png" for k in ("tree", "fire", "empty"))
    if force or not tree.exists():
        log.info("keyframe: tree")
        generate(KEY_PROMPTS["tree"], {}, 1, seed)[0].save(tree)
    if force or not fire.exists():
        # The model takes its time igniting, so use the most-burning frame
        # rather than a fixed point in the clip.
        log.info("keyframe: fire (burn clip)")
        frames = generate(KEY_PROMPTS["burn"], {0: Image.open(tree).convert("RGB")}, 49, seed + 1)
        save_clip("_burn", frames)
        max(frames, key=fire_fraction).save(fire)
    if force or not empty.exists():
        log.info("keyframe: empty")
        generate(KEY_PROMPTS["empty"], {}, 1, seed + 2)[0].save(empty)


def fire_fraction(im: Image.Image) -> float:
    a = np.asarray(im.convert("RGB")).astype(int)
    r, g, b = a[..., 0], a[..., 1], a[..., 2]
    return float(((r > 200) & (g > 100) & (r - b > 120)).mean())


def stage_clips(names: list[str], seed: int, force: bool, lease: turnstile.Lease | None) -> turnstile.Lease | None:
    keys = {k: Image.open(KEYFRAMES / f"{k}.png").convert("RGB") for k in ("empty", "tree", "fire")}
    for i, name in enumerate(names):
        spec = CLIPS[name]
        if not force and (CLIPS_DIR / name).is_dir():
            log.info("clip %s: exists, skipping", name)
            continue
        if lease and lease.revoke_requested:
            log.info("yielding the GPU to a higher-priority job")
            lease.release()
            lease = turnstile.acquire(est_seconds=180 * (len(names) - i))
        log.info("clip %s", name)
        a, b = keys[spec["start"]], keys[spec["end"]]
        anchors = {0: a, NUM_FRAMES - 1: b}
        if "wipe" in spec:
            anchors[NUM_FRAMES // 2] = wipe(a, b, spec["wipe"])
        save_clip(name, generate(spec["prompt"], anchors, NUM_FRAMES, seed + 100 + i))
    return lease


def stage_pack(frames: int, tile: int, loop_seconds: float):
    rows = []
    for name, spec in CLIPS.items():
        src = sorted((CLIPS_DIR / name).glob("*.png"))
        if not src:
            raise SystemExit(f"clip {name} has no frames; run the clips stage first")
        if spec.get("loop"):
            # The last frame repeats the first; drop it so the loop is seamless.
            src = src[:-1]
            idx = np.linspace(0, len(src), frames, endpoint=False)
        else:
            idx = np.linspace(0, len(src) - 1, frames)
        rows.append([Image.open(src[round(j) % len(src)]).convert("RGB").resize((tile, tile), Image.LANCZOS) for j in idx])

    sheet = Image.new("RGB", (frames * tile, len(rows) * tile))
    for r, row in enumerate(rows):
        for f, im in enumerate(row):
            sheet.paste(im, (f * tile, r * tile))
    OUT.mkdir(parents=True, exist_ok=True)
    sheet.save(OUT / "atlas.png", optimize=True)
    manifest = {
        "tile": tile,
        "clips": {
            name: {"row": r, "frames": frames, **({"seconds": loop_seconds} if spec.get("loop") else {})}
            for r, (name, spec) in enumerate(CLIPS.items())
        },
    }
    (OUT / "atlas.json").write_text(json.dumps(manifest, indent=2) + "\n")
    log.info("wrote %s (%dx%d)", OUT / "atlas.png", *sheet.size)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("stage", nargs="?", default="all", choices=["keyframes", "clips", "pack", "all"])
    ap.add_argument("--only", nargs="+", choices=list(CLIPS), help="clips to (re)generate")
    ap.add_argument("--force", action="store_true", help="regenerate outputs that already exist")
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--frames", type=int, default=32, help="atlas frames per clip")
    ap.add_argument("--tile", type=int, default=128, help="atlas tile size in pixels")
    ap.add_argument("--loop-seconds", type=float, default=2.5)
    args = ap.parse_args()
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(name)s %(message)s")
    logging.getLogger("httpx").setLevel(logging.WARNING)

    names = args.only or list(CLIPS)
    force = args.force or bool(args.only)
    if args.stage in ("keyframes", "clips", "all"):
        from huggingface_hub import snapshot_download

        # Fetch weights before queueing for the GPU, not while holding it.
        snapshot_download(MODEL)
        lease = turnstile.acquire(est_seconds=180 * (len(names) + 2))
        try:
            if args.stage in ("keyframes", "all"):
                stage_keyframes(args.seed, args.force)
            if args.stage in ("clips", "all"):
                lease = stage_clips(names, args.seed, force, lease)
        finally:
            if lease:
                lease.release()
    if args.stage in ("pack", "all"):
        stage_pack(args.frames, args.tile, args.loop_seconds)


if __name__ == "__main__":
    main()
