//! The "Lock to critical" curve: lightning as a function of growth and grid
//! size, fitted to `tinderbox bench` sweeps by `tools/bench/fit.py`.
//!
//! For a grid and a growth rate p, θ* is the largest p/f at which fires stay
//! off the grid scale (under 5% of burnt trees burn in fires over a tenth of
//! the grid). Raising θ lengthens the power law in fire sizes, until fires
//! start sweeping the whole grid; θ* is about as long as it gets. Above
//! `P_MAX`, trees regrow behind a front fast enough to keep fires alive.

/// Growth is capped here while locked.
pub const P_MAX: f32 = 0.003;

/// (p, a, b): θ* = exp(a) · cells^b, from 16 K to 1 M cells, 20 000 fires a
/// run. θ* is interpolated between rows in ln p; below the first row it's held.
const N4: [(f64, f64, f64); 4] = [
    (1e-4, -7.348, 1.0245),
    (3e-4, -5.893, 0.9483),
    (1e-3, -1.716, 0.5913),
    (3e-3, 0.815, 0.3248),
];
// Not measured yet; the 4-neighbor fit stands in.
const N8: [(f64, f64, f64); 4] = N4;

/// Returns (growth, lightning) on the curve for a grid of `cells` cells.
pub fn lock(growth: f32, cells: u32, moore: bool) -> (f32, f32) {
    let p = growth.min(P_MAX);
    (p, p / theta(p as f64, cells, moore) as f32)
}

/// θ* for growth `p` on a grid of `cells` cells.
pub fn theta(p: f64, cells: u32, moore: bool) -> f64 {
    let rows = if moore { &N8 } else { &N4 };
    let lp = p.clamp(rows[0].0, rows[rows.len() - 1].0).ln();
    let i = rows.windows(2).position(|w| lp <= w[1].0.ln()).unwrap_or(rows.len() - 2);
    let (r0, r1) = (rows[i], rows[i + 1]);
    let t = (lp - r0.0.ln()) / (r1.0.ln() - r0.0.ln());
    let (a, b) = (r0.1 + t * (r1.1 - r0.1), r0.2 + t * (r1.2 - r0.2));
    (a + b * (cells.max(1) as f64).ln()).exp().max(2.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_table_at_its_rows() {
        let cells = 512 * 512;
        for (p, a, b) in N4 {
            let want = (a + b * (cells as f64).ln()).exp();
            assert!((theta(p, cells, false) / want - 1.0).abs() < 1e-9);
        }
    }

    #[test]
    fn caps_growth_and_grows_with_the_grid() {
        let (p, f) = lock(0.01, 640 * 360, false);
        assert_eq!(p, P_MAX);
        assert!(f > 0.0 && p / f > 50.0 && p / f < 500.0, "θ {}", p / f);
        assert!(theta(3e-4, 1 << 20, false) > theta(3e-4, 1 << 14, false));
        // Held below the slowest measured growth.
        assert_eq!(theta(1e-6, 1 << 16, false), theta(1e-4, 1 << 16, false));
    }
}
