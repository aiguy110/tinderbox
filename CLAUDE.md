# tinderbox: agent notes

A WebGL2 Drossel–Schwabl forest-fire sim with diffusion-generated tile animation. Start with `README.md` (what it is and how to run it) and **`docs/PLAN.md`**, which has the current plan, design, status, next steps and environment gotchas.

## Keep the plan current

`docs/PLAN.md` is the hand-off document. A new agent should be able to resume from it alone. Update it whenever you:

- finish or start a milestone (tick boxes; rewrite **Next steps**)
- make or reverse a design decision (record the decision and why)
- learn something non-obvious about the models, the GPU or the environment (add it to **Environment notes** or **Log**)

Update it in the same commit as the work, and before ending a session.

## SOP: commit and merge often

- Commit small, coherent steps as soon as they work (typecheck/build passing). Don't sit on a large uncommitted diff.
- Merge into `main` frequently: after each working step, not just at milestone ends. Work usually happens on a worktree branch while `main` is checked out at `~/Projects/tinderbox`. From there, run `git merge --ff-only <branch>`. If `main` has moved, rebase the branch onto `main` first.

## Binary assets live outside the repo

Generated atlases, keyframes, clips and logs go in `$TINDERBOX_DATA` (default `~/.local/share/tinderbox`):

- `atlas/` is served at `/atlas/` by `vite.config.ts` and copied into `dist/` on build.
- `work/` holds generator scratch (keyframes, clips, logs).

Don't commit generated images or video. Model weights stay in the Hugging Face cache.

## The live app is the tailnet build, kept current by a systemd unit

The app the user looks at is `https://bifrost.tail2280a0.ts.net:8451/`. Tailscale serves `~/Projects/tinderbox/dist` there as static files. `tinderbox-build.service` (a user unit, source in `tools/systemd/`) runs `vite build --watch` in the **main checkout**, so `dist/` rebuilds whenever `main` changes.

**SOP: after merging into `main`, confirm the live app updated.** Check `journalctl --user -u tinderbox-build -n 20` for a fresh `built in` line, or check that `curl -sk https://bifrost.tail2280a0.ts.net:8451/` names the new `index-*.js`. If the unit is down, run `npm run build` in `~/Projects/tinderbox` and fix the unit. Tell the user to hard-refresh.

- Status and logs: `systemctl --user status tinderbox-build`, `journalctl --user -u tinderbox-build -f`.
- Atlas files under `$TINDERBOX_DATA/atlas/` are copied into `dist/` on each build. After regenerating an atlas, `systemctl --user restart tinderbox-build`.
- If the unit file changes: `systemctl --user daemon-reload && systemctl --user restart tinderbox-build`.
- No dev server runs permanently. To preview a branch, run `npx vite --port <port>` from the worktree and stop it when you're done. Find it with `ps -eo pid,args | grep "[v]ite --port"`, because `pkill -f` matches its own shell.

## Commands

```sh
npm run dev          # vite dev server
npm run typecheck
npm run build
cd tools/atlas && HF_HUB_OFFLINE=1 .venv/bin/python generate.py --help   # v0 generator
```
