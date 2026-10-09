#!/usr/bin/env python3
"""Fit the "Lock to critical" curve from `tinderbox bench` sweeps.

For each (grid, neighborhood, p), theta* is the largest p/f at which fires
still mostly stay off the grid scale: the share of burnt trees that burnt in
fires over a tenth of the grid stays under SPAN (interpolated in log theta).
Then, for each p, fits ln theta* = a + b ln cells by least squares, and
prints the (p, a, b) rows as a Rust table for src/critical.rs, which
interpolates between them in ln p. (theta* peaks at middling p, and where
depends on the grid: slower growth lets fires span small grids, faster growth
refeeds fires from behind. One smooth surface in both fit badly.)

usage: fit.py SWEEP.tsv [SPAN]
"""

import math
import sys
from collections import defaultdict


def main():
    path = sys.argv[1]
    span = float(sys.argv[2]) if len(sys.argv) > 2 else 0.05
    rows = []
    with open(path) as f:
        head = f.readline().split()
        for line in f:
            r = dict(zip(head, line.split()))
            rows.append(r)
    groups = defaultdict(list)
    for r in rows:
        key = (int(r["w"]) * int(r["h"]), int(r["moore"]), float(r["p"]))
        groups[key].append((float(r["theta"]), float(r["spanning"]), float(r["decades"]), float(r["tau"])))

    pts = []
    print("cells\tmoore\tp\ttheta*\tdecades\ttau")
    for (cells, moore, p), runs in sorted(groups.items()):
        runs.sort()
        star = None
        for (t0, s0, d0, _), (t1, s1, _, _) in zip(runs, runs[1:]):
            if s0 <= span < s1:
                x = (span - s0) / (s1 - s0)
                star = math.exp(math.log(t0) + x * (math.log(t1) - math.log(t0)))
                break
        if star is None:
            print(f"{cells}\t{moore}\t{p}\t(no crossing: spanning {runs[0][1]:.3f}..{runs[-1][1]:.3f})")
            continue
        # The run nearest theta* from below, for the decades it got.
        below = [r for r in runs if r[0] <= star]
        d, tau = (below[-1][2], below[-1][3]) if below else (0, 0)
        print(f"{cells}\t{moore}\t{p}\t{star:.0f}\t{d:.2f}\t{tau:.2f}")
        pts.append((moore, math.log(cells), math.log(p), math.log(star)))

    for moore in sorted({m for m, *_ in pts}):
        print(f"\n// moore = {moore}: (p, a, b), theta* = exp(a) * cells^b")
        worst = 0.0
        for p in sorted({p for m, _, p, _ in pts if m == moore}):
            xs = [(1.0, n) for m, n, q, _ in pts if m == moore and q == p]
            ys = [t for m, _, q, t in pts if m == moore and q == p]
            if len(xs) < 2:
                continue
            a, b = lstsq(xs, ys)
            worst = max([worst] + [abs(y - a - b * n) for (_, n), y in zip(xs, ys)])
            print(f"    ({math.exp(p):.0e}, {a:.3f}, {b:.4f}),")
        print(f"// worst residual {worst:.3f} in ln theta*")


def lstsq(xs, ys):
    """Normal equations, solved by Gauss-Jordan."""
    k = len(xs[0])
    a = [[sum(r[i] * r[j] for r in xs) for j in range(k)] for i in range(k)]
    b = [sum(r[i] * y for r, y in zip(xs, ys)) for i in range(k)]
    for i in range(k):
        piv = max(range(i, k), key=lambda r: abs(a[r][i]))
        a[i], a[piv], b[i], b[piv] = a[piv], a[i], b[piv], b[i]
        for r in range(k):
            if r != i:
                m = a[r][i] / a[i][i]
                a[r] = [x - m * y for x, y in zip(a[r], a[i])]
                b[r] -= m * b[i]
    return [b[i] / a[i][i] for i in range(k)]


if __name__ == "__main__":
    main()
