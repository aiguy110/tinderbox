# tinderbox: agent notes

A Rust (wgpu) Drossel–Schwabl forest-fire sim with a procedural scene renderer, for the web (wasm) and native multi-monitor. Start with `README.md` (what it is and how to run it) and **`docs/PLAN.md`**, which has the current plan, design, status, next steps and environment gotchas.

## Keep the plan current

`docs/PLAN.md` is the hand-off document. A new agent should be able to resume from it alone. Update it whenever you:

- finish or start a milestone (tick boxes; rewrite **Next steps**)
- make or reverse a design decision (record the decision and why)
- learn something non-obvious about the models, the GPU or the environment (add it to **Environment notes** or **Log**)

Update it in the same commit as the work, and before ending a session.

## SOP: commit and merge often

- Commit small, coherent steps as soon as they work (`cargo clippy` for both targets passing). Don't sit on a large uncommitted diff.
- Merge into `main` frequently: after each working step, not just at milestone ends. Work usually happens on a worktree branch while `main` is checked out at `~/Projects/tinderbox`. From there, run `git merge --ff-only <branch>`. If `main` has moved, rebase the branch onto `main` first.

## Binary assets live outside the repo

Generated atlases, keyframes, clips and logs go in `$TINDERBOX_DATA` (default `~/.local/share/tinderbox`):

- `atlas/` held tile atlases for the old tiles renderer; nothing reads it since the Rust port.
- `work/` holds generator scratch (keyframes, clips, logs).

Don't commit generated images or video. Model weights stay in the Hugging Face cache.

## The live app is the tailnet build, kept current by a systemd unit

The app the user looks at is `https://bifrost.tail2280a0.ts.net:8451/`. Tailscale serves `~/Projects/tinderbox/dist` there as static files. `tinderbox-build.service` (a user unit, source in `tools/systemd/`) runs `trunk watch --release` in the **main checkout**, so `dist/` rebuilds whenever `main` changes (about 40 s).

**SOP: after merging into `main`, confirm the live app updated.** Check `journalctl --user -u tinderbox-build -n 20` for a fresh `success` line, or check that `curl -sk https://bifrost.tail2280a0.ts.net:8451/` names the new `tinderbox-*.js`. If the unit is down, run `trunk build --release` in `~/Projects/tinderbox` and fix the unit. Tell the user to hard-refresh.

- Status and logs: `systemctl --user status tinderbox-build`, `journalctl --user -u tinderbox-build -f`.
- If the unit file changes: `systemctl --user daemon-reload && systemctl --user restart tinderbox-build`.
- No dev server runs permanently. To preview a branch, run `trunk serve --port <port>` from the worktree and stop it when you're done. Find it with `ps -eo pid,args | grep "[t]runk serve"`, because `pkill -f` matches its own shell.

## Commands

```sh
cargo run --release -- --windowed                    # native (no display on bifrost: see PLAN.md Environment notes)
cargo run --release -- --shot /tmp/shot --layout 1280x720+0+0,960x720+1280+0   # headless PNGs
trunk serve                                          # web dev server
cargo clippy && cargo clippy --target wasm32-unknown-unknown
cd tools/atlas && HF_HUB_OFFLINE=1 .venv/bin/python generate.py --help   # v0 generator
```
