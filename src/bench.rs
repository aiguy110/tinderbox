//! `tinderbox bench`: how scale-free the fires are for given settings.
//!
//! A CPU replica of the step rule in `shaders/step.wgsl` (catch, burn,
//! burnt, clear, grow), run event by event: only burning cells, growth and
//! strikes cost anything, and quiet spells between fires are skipped whole.
//! Each fire carries an id (merged with union-find when two fires meet), and
//! its size is the number of trees it burned. The score is how many decades
//! of the fire-size distribution are a straight line on log-log axes.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use crate::rng::Rng;

const EMPTY: u8 = 0;
const TREE: u8 = 1;
/// Caught this step; becomes FIRE_A at the end of it.
const CATCH: u8 = 2;
/// The first and second steps on fire (both spread).
const FIRE_A: u8 = 3;
const FIRE_B: u8 = 4;
const BURNT: u8 = 5;

const N4: [(i32, i32); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];
const N8: [(i32, i32); 8] = [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)];

#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub w: u32,
    pub h: u32,
    pub moore: bool,
    pub wrap: bool,
    pub growth: f64,
    /// p/f.
    pub theta: f64,
}

pub struct Run {
    pub sizes: Vec<u32>,
    pub steps: u64,
    /// Mean tree density, sampled when each fire starts.
    pub density: f64,
}

struct Fires {
    parent: Vec<u32>,
    size: Vec<u32>,
    active: Vec<u32>,
}

impl Fires {
    fn find(&mut self, mut i: u32) -> u32 {
        while self.parent[i as usize] != i {
            let g = self.parent[self.parent[i as usize] as usize];
            self.parent[i as usize] = g;
            i = g;
        }
        i
    }

    fn new_fire(&mut self) -> u32 {
        let i = self.parent.len() as u32;
        self.parent.push(i);
        self.size.push(0);
        self.active.push(0);
        i
    }

    fn union(&mut self, a: u32, b: u32) -> u32 {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.parent[b as usize] = a;
            self.size[a as usize] += self.size[b as usize];
            self.active[a as usize] += self.active[b as usize];
        }
        a
    }
}

struct Sim {
    c: Config,
    state: Vec<u8>,
    id: Vec<u32>,
    fires: Fires,
    caught: Vec<u32>,
    burning: Vec<u32>,
    burnt: Vec<u32>,
    trees: u64,
    rng: Rng,
}

impl Sim {
    fn new(c: Config, seed: u64) -> Self {
        let n = (c.w * c.h) as usize;
        let mut rng = Rng::new(seed);
        // Start near the steady-state density so warm-up is short.
        let rho = if c.moore { 0.3 } else { 0.4 };
        let state: Vec<u8> = (0..n).map(|_| if rng.f64() < rho { TREE } else { EMPTY }).collect();
        let trees = state.iter().filter(|&&s| s == TREE).count() as u64;
        Sim {
            c,
            state,
            id: vec![0; n],
            fires: Fires { parent: vec![], size: vec![], active: vec![] },
            caught: vec![],
            burning: vec![],
            burnt: vec![],
            trees,
            rng,
        }
    }

    fn cells(&self) -> u32 {
        self.c.w * self.c.h
    }

    fn neighbor(&self, i: u32, (dx, dy): (i32, i32)) -> Option<u32> {
        let (w, h) = (self.c.w as i32, self.c.h as i32);
        let (mut x, mut y) = ((i % self.c.w) as i32 + dx, (i / self.c.w) as i32 + dy);
        if self.c.wrap {
            x = (x + w) % w;
            y = (y + h) % h;
        } else if x < 0 || y < 0 || x >= w || y >= h {
            return None;
        }
        Some((y * w + x) as u32)
    }

    fn ignite(&mut self, i: u32, fire: u32) {
        self.state[i as usize] = CATCH;
        self.id[i as usize] = fire;
        self.fires.size[fire as usize] += 1;
        self.fires.active[fire as usize] += 1;
        self.trees -= 1;
        self.caught.push(i);
    }

    fn grow(&mut self, picks: u64) {
        for _ in 0..picks {
            let i = self.rng.below(self.cells()) as usize;
            if self.state[i] == EMPTY {
                self.state[i] = TREE;
                self.trees += 1;
            }
        }
    }

    /// One step. Returns sizes of fires that went out, and whether a strike
    /// started a fire.
    fn step(&mut self, out: &mut Vec<u32>) -> bool {
        let n = self.cells() as f64;
        let neighbors: &[(i32, i32)] = if self.c.moore { &N8 } else { &N4 };
        // Spread from everything on fire at the start of the step.
        let front: Vec<u32> = self.caught.drain(..).chain(self.burning.iter().copied()).collect();
        let was_caught = front.len() - self.burning.len();
        for &i in &front {
            for &d in neighbors {
                let Some(j) = self.neighbor(i, d) else { continue };
                match self.state[j as usize] {
                    TREE => {
                        let f = self.fires.find(self.id[i as usize]);
                        self.ignite(j, f);
                    }
                    CATCH => {
                        let (a, b) = (self.id[i as usize], self.id[j as usize]);
                        self.fires.union(a, b);
                    }
                    _ => {}
                }
            }
        }
        // Lightning, on trees that didn't catch from a neighbor.
        let mut struck = false;
        for _ in 0..poisson(&mut self.rng, n * self.c.growth / self.c.theta) {
            let i = self.rng.below(self.cells());
            if self.state[i as usize] == TREE {
                let f = self.fires.new_fire();
                self.ignite(i, f);
                struck = true;
            }
        }
        // Growth sees the state at the start of the step, so cells that clear
        // or catch during it aren't eligible; both are already non-EMPTY.
        let g = poisson(&mut self.rng, n * self.c.growth);
        self.grow(g);

        for i in self.burnt.drain(..) {
            self.state[i as usize] = EMPTY;
        }
        for &i in &self.burning {
            self.state[i as usize] = BURNT;
            self.burnt.push(i);
            let f = self.fires.find(self.id[i as usize]);
            self.fires.active[f as usize] -= 1;
            if self.fires.active[f as usize] == 0 {
                out.push(self.fires.size[f as usize]);
            }
        }
        self.burning.clear();
        // Last step's catches burn a second step.
        for &i in &front[..was_caught] {
            self.state[i as usize] = FIRE_B;
            self.burning.push(i);
        }
        for &i in &self.caught {
            self.state[i as usize] = FIRE_A;
        }
        if self.fires.parent.len() > 1 << 20 {
            self.compact();
        }
        struck
    }

    fn quiet(&self) -> bool {
        self.caught.is_empty() && self.burning.is_empty() && self.burnt.is_empty()
    }

    /// Skip to just before the next strike: grow for the steps in between.
    fn skip(&mut self) -> u64 {
        let rate = self.cells() as f64 * self.c.growth / self.c.theta;
        let gap = (-(1.0 - self.rng.f64()).ln() / rate).floor() as u64;
        let mean = self.cells() as f64 * self.c.growth * gap as f64;
        let g = poisson(&mut self.rng, mean);
        self.grow(g);
        gap
    }

    /// Renumber the fires still burning so the id tables stay small.
    fn compact(&mut self) {
        let mut map = std::collections::HashMap::new();
        let mut fires = Fires { parent: vec![], size: vec![], active: vec![] };
        let cells: Vec<u32> = self.caught.iter().chain(&self.burning).copied().collect();
        for i in cells {
            let r = self.fires.find(self.id[i as usize]);
            let n = *map.entry(r).or_insert_with(|| {
                let n = fires.new_fire();
                fires.size[n as usize] = self.fires.size[r as usize];
                fires.active[n as usize] = self.fires.active[r as usize];
                n
            });
            self.id[i as usize] = n;
        }
        self.fires = fires;
    }
}

fn poisson(rng: &mut Rng, mean: f64) -> u64 {
    if mean < 30.0 {
        return rng.poisson(mean) as u64;
    }
    let g = (-2.0 * (1.0 - rng.f64()).ln()).sqrt() * (std::f64::consts::TAU * rng.f64()).cos();
    (mean + mean.sqrt() * g).round().max(0.0) as u64
}

/// Simulate until `fires` fires have been recorded after a warm-up (or the
/// time budget runs out).
pub fn run(c: Config, fires: usize, secs: f64, seed: u64) -> Run {
    let start = Instant::now();
    let mut sim = Sim::new(c, seed);
    let warm_steps = (10.0 / c.growth) as u64;
    let warm_fires = 500;
    let mut steps = 0u64;
    let mut out = Vec::new();
    let mut sizes = Vec::with_capacity(fires);
    let mut warm = true;
    let mut seen = 0usize;
    let (mut rho_sum, mut rho_n) = (0.0, 0u64);
    while sizes.len() < fires && start.elapsed().as_secs_f64() < secs {
        if sim.quiet() {
            steps += sim.skip();
        }
        let n = sim.cells() as f64;
        if sim.step(&mut out) && !warm {
            rho_sum += sim.trees as f64 / n;
            rho_n += 1;
        }
        steps += 1;
        if warm {
            seen += out.len();
            out.clear();
            if steps >= warm_steps && seen >= warm_fires {
                warm = false;
                steps = 0;
            }
        } else {
            sizes.append(&mut out);
        }
    }
    Run { sizes, steps, density: rho_sum / rho_n.max(1) as f64 }
}

pub struct Fit {
    /// Decades of fire size over which the distribution is a straight line.
    pub decades: f64,
    /// Its exponent: P(s) ~ s^-tau.
    pub tau: f64,
    pub lo: f64,
    pub hi: f64,
    /// Largest fire as a fraction of the grid.
    pub largest: f64,
    /// Share of all burnt trees that burnt in fires over a tenth of the grid.
    pub spanning: f64,
}

/// Log-binned density (5 bins per decade), then the longest run of bins with
/// at least `MIN_COUNT` fires each that a least-squares line fits to within
/// `TOL` (log10 units) everywhere.
pub fn fit(sizes: &[u32], cells: u32) -> Fit {
    const PER_DECADE: f64 = 5.0;
    const MIN_COUNT: u32 = 20;
    const TOL: f64 = 0.06;
    let max = sizes.iter().copied().max().unwrap_or(1);
    let nbins = ((max as f64).log10() * PER_DECADE).floor() as usize + 1;
    let edge = |k: usize| 10f64.powf(k as f64 / PER_DECADE);
    let mut counts = vec![0u32; nbins];
    for &s in sizes {
        counts[(((s as f64).log10() * PER_DECADE + 1e-9).floor() as usize).min(nbins - 1)] += 1;
    }
    // (log10 s, log10 density) per usable bin, keeping bin indices.
    let pts: Vec<(usize, f64, f64)> = (0..nbins)
        .filter_map(|k| {
            let (lo, hi) = (edge(k).ceil(), edge(k + 1).ceil());
            let ints = hi - lo;
            (counts[k] >= MIN_COUNT && ints > 0.0)
                .then(|| (k, ((lo * (hi - 1.0)).sqrt()).log10(), (counts[k] as f64 / ints / sizes.len() as f64).log10()))
        })
        .collect();
    let mut best = (0.0, 0.0, 1.0, 1.0);
    for i in 0..pts.len() {
        for j in i + 2..pts.len() {
            if pts[j].0 - pts[i].0 != j - i {
                break; // a gap in usable bins
            }
            let run = &pts[i..=j];
            let m = run.len() as f64;
            let (sx, sy) = run.iter().fold((0.0, 0.0), |(a, b), p| (a + p.1, b + p.2));
            let (mx, my) = (sx / m, sy / m);
            let sxx: f64 = run.iter().map(|p| (p.1 - mx).powi(2)).sum();
            let sxy: f64 = run.iter().map(|p| (p.1 - mx) * (p.2 - my)).sum();
            let slope = sxy / sxx;
            let worst = run.iter().map(|p| (p.2 - my - slope * (p.1 - mx)).abs()).fold(0.0, f64::max);
            let (lo, hi) = (edge(pts[i].0), edge(pts[j].0 + 1));
            let decades = (hi / lo).log10();
            if worst <= TOL && decades > best.0 {
                best = (decades, -slope, lo, hi);
            }
        }
    }
    let total: f64 = sizes.iter().map(|&s| s as f64).sum();
    let big: f64 = sizes.iter().filter(|&&s| s > cells / 10).map(|&s| s as f64).sum();
    Fit {
        decades: best.0,
        tau: best.1,
        lo: best.2,
        hi: best.3,
        largest: max as f64 / cells as f64,
        spanning: if total > 0.0 { big / total } else { 0.0 },
    }
}

const USAGE: &str = "\
usage: tinderbox bench [options]

Runs the sim on the CPU for every combination of the lists below and prints
one tab-separated line per run: how many decades of fire size follow a power
law, its exponent, and how close fires come to spanning the grid.

  --grid WxH,...     grid sizes in cells (default 256x256)
  --theta X,...      p/f, growth over lightning (default 100,1000,10000)
  --p X,...          growth per step (default 0.001)
  --moore            8-neighbor spread (--both for 4 and 8)
  --no-wrap          hard edges
  --fires N          fires to record per run (default 20000)
  --secs S           time budget per run (default 300)
  --threads N        runs at once (default: all cores)
  --sizes DIR        also write each run's fire sizes to DIR
  --resume FILE      skip runs already in FILE (an earlier sweep's output)
                     and leave out the header, for appending to it";

fn list<T: std::str::FromStr>(args: &[String], flag: &str, default: &str) -> Result<Vec<T>, String> {
    let v = args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).map(String::as_str).unwrap_or(default);
    v.split(',').map(|s| s.trim().parse().map_err(|_| format!("bad {flag} value {s:?}\n\n{USAGE}"))).collect()
}

pub fn main(args: &[String]) -> Result<(), String> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return Ok(());
    }
    let grids: Vec<(u32, u32)> = list::<String>(args, "--grid", "256x256")?
        .iter()
        .map(|g| {
            let (w, h) = g.split_once('x').ok_or(format!("--grid takes WxH, not {g:?}"))?;
            Ok((w.parse().map_err(|_| "bad --grid")?, h.parse().map_err(|_| "bad --grid")?))
        })
        .collect::<Result<_, String>>()?;
    let thetas: Vec<f64> = list(args, "--theta", "100,1000,10000")?;
    let ps: Vec<f64> = list(args, "--p", "0.001")?;
    let fires: usize = list(args, "--fires", "20000")?[0];
    let secs: f64 = list(args, "--secs", "300")?[0];
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let threads: usize = list(args, "--threads", &cores.to_string())?[0];
    let sizes_dir = args.iter().position(|a| a == "--sizes").and_then(|i| args.get(i + 1)).cloned();
    let moores: Vec<bool> = if args.iter().any(|a| a == "--both") {
        vec![false, true]
    } else {
        vec![args.iter().any(|a| a == "--moore")]
    };
    let wrap = !args.iter().any(|a| a == "--no-wrap");
    if let Some(d) = &sizes_dir {
        std::fs::create_dir_all(d).map_err(|e| format!("{d}: {e}"))?;
    }

    // (w, h, moore, p, theta) as printed, of runs already done.
    let done: std::collections::HashSet<String> = match args.iter().position(|a| a == "--resume").and_then(|i| args.get(i + 1)) {
        Some(f) => std::fs::read_to_string(f)
            .map_err(|e| format!("{f}: {e}"))?
            .lines()
            .skip(1)
            .map(|l| l.split('\t').take(5).collect::<Vec<_>>().join("\t"))
            .collect(),
        None => Default::default(),
    };
    let key = |c: &Config| format!("{}\t{}\t{}\t{}\t{}", c.w, c.h, c.moore as u8, c.growth, c.theta);
    let mut configs = vec![];
    for &(w, h) in &grids {
        for &moore in &moores {
            for &growth in &ps {
                for &theta in &thetas {
                    let c = Config { w, h, moore, wrap, growth, theta };
                    if !done.contains(&key(&c)) {
                        configs.push(c);
                    }
                }
            }
        }
    }
    if done.is_empty() {
        println!("w\th\tmoore\tp\ttheta\tfires\tsteps\tdensity\tdecades\ttau\tlo\thi\tlargest\tspanning\tsecs");
    }
    eprintln!("{} runs", configs.len());
    let next = AtomicUsize::new(0);
    let print = Mutex::new(());
    let seed = Rng::from_clock().next_u64();
    std::thread::scope(|s| {
        for _ in 0..threads.max(1) {
            s.spawn(|| {
                loop {
                    let k = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&c) = configs.get(k) else { break };
                    let t = Instant::now();
                    let r = run(c, fires, secs, seed ^ (k as u64).wrapping_mul(0x9e3779b97f4a7c15));
                    let f = fit(&r.sizes, c.w * c.h);
                    if let Some(d) = &sizes_dir {
                        let name = format!("{d}/{}x{}-{}-p{}-t{}.txt", c.w, c.h, if c.moore { 8 } else { 4 }, c.growth, c.theta);
                        let body: String = r.sizes.iter().map(|s| format!("{s}\n")).collect();
                        let _ = std::fs::write(name, body);
                    }
                    let _g = print.lock();
                    println!(
                        "{}\t{}\t{}\t{:.3}\t{:.2}\t{:.3}\t{:.0}\t{:.0}\t{:.4}\t{:.3}\t{:.0}",
                        key(&c),
                        r.sizes.len(),
                        r.steps,
                        r.density,
                        f.decades,
                        f.tau,
                        f.lo,
                        f.hi,
                        f.largest,
                        f.spanning,
                        t.elapsed().as_secs_f64()
                    );
                }
            });
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_a_pure_power_law() {
        // Inverse-transform samples of a discrete-ish s^-1.5 tail.
        let mut rng = Rng::new(7);
        let sizes: Vec<u32> = (0..200_000).map(|_| (1.0 / (1.0 - rng.f64()).powf(2.0)).floor().min(1e9) as u32).collect();
        let f = fit(&sizes, u32::MAX);
        assert!((f.tau - 1.5).abs() < 0.1, "tau {}", f.tau);
        assert!(f.decades > 2.5, "decades {}", f.decades);
    }

    #[test]
    fn fires_are_counted_once_each() {
        let c = Config { w: 64, h: 64, moore: false, wrap: true, growth: 0.01, theta: 100.0 };
        let r = run(c, 2000, 30.0, 1);
        assert_eq!(r.sizes.len(), 2000);
        assert!(r.sizes.iter().all(|&s| (1..=64 * 64 * 4).contains(&s)));
        assert!(r.density > 0.1 && r.density < 0.8, "density {}", r.density);
    }
}
