//! A small PRNG (SplitMix64), seeded from the clock. Nothing here needs to be
//! cryptographic, and it avoids getrandom's wasm setup.

pub struct Rng(u64);

impl Rng {
    pub fn from_clock() -> Self {
        let t = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1);
        Rng(t ^ 0x9e3779b97f4a7c15)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }

    pub fn u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    /// Uniform in [0, 1).
    pub fn f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    pub fn f32(&mut self) -> f32 {
        self.f64() as f32
    }

    pub fn below(&mut self, n: u32) -> u32 {
        (self.f64() * n as f64) as u32
    }

    pub fn poisson(&mut self, mean: f64) -> u32 {
        let l = (-mean).exp();
        let mut k = 0;
        let mut p = self.f64();
        while p > l {
            k += 1;
            p *= self.f64();
        }
        k
    }
}
