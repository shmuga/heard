//! Planar f32 audio buffer used by the whole pipeline (the sound version of Found's `Img`).
//!
//! Samples are nominally in -1..1; ops may go over (headroom is kept until the WAV is
//! written, where it's clipped). Channel count is 1 or 2 in practice, but nothing assumes it.

use rayon::prelude::*;

#[derive(Clone, Debug, PartialEq)]
pub struct Audio {
    pub rate: u32,
    /// One Vec per channel, all the same length.
    pub ch: Vec<Vec<f32>>,
}

impl Audio {
    pub fn new(rate: u32, channels: usize, frames: usize) -> Self {
        Self {
            rate,
            ch: vec![vec![0.0; frames]; channels.max(1)],
        }
    }

    pub fn from_channels(rate: u32, ch: Vec<Vec<f32>>) -> Self {
        let mut a = Self { rate, ch };
        if a.ch.is_empty() {
            a.ch.push(vec![]);
        }
        let n = a.ch.iter().map(|c| c.len()).min().unwrap_or(0);
        for c in &mut a.ch {
            c.truncate(n);
        }
        a
    }

    pub fn mono(rate: u32, s: Vec<f32>) -> Self {
        Self { rate, ch: vec![s] }
    }

    pub fn frames(&self) -> usize {
        self.ch.first().map(|c| c.len()).unwrap_or(0)
    }

    pub fn channels(&self) -> usize {
        self.ch.len()
    }

    pub fn seconds(&self) -> f64 {
        self.frames() as f64 / self.rate.max(1) as f64
    }

    pub fn secs_to_frame(&self, s: f64) -> usize {
        ((s.max(0.0) * self.rate as f64).round() as usize).min(self.frames())
    }

    /// Mono mix (average of channels).
    pub fn mix_mono(&self) -> Vec<f32> {
        let n = self.frames();
        let k = self.channels().max(1) as f32;
        (0..n)
            .into_par_iter()
            .map(|i| self.ch.iter().map(|c| c[i]).sum::<f32>() / k)
            .collect()
    }

    /// Frames `a..b` (clamped) as a new buffer.
    pub fn slice(&self, a: usize, b: usize) -> Audio {
        let n = self.frames();
        let (a, b) = (a.min(n), b.min(n).max(a.min(n)));
        Audio {
            rate: self.rate,
            ch: self.ch.iter().map(|c| c[a..b].to_vec()).collect(),
        }
    }

    pub fn peak(&self) -> f32 {
        self.ch
            .par_iter()
            .map(|c| c.iter().fold(0.0f32, |m, v| m.max(v.abs())))
            .reduce(|| 0.0, f32::max)
    }

    pub fn rms(&self) -> f32 {
        let n = (self.frames() * self.channels()).max(1) as f64;
        let s: f64 = self
            .ch
            .par_iter()
            .map(|c| c.iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>())
            .sum();
        (s / n).sqrt() as f32
    }

    pub fn gain(&mut self, g: f32) {
        self.ch
            .par_iter_mut()
            .for_each(|c| c.iter_mut().for_each(|v| *v *= g));
    }

    /// Apply `f` to every channel in parallel.
    pub fn map_channels(&mut self, f: impl Fn(&mut Vec<f32>) + Sync + Send) {
        self.ch.par_iter_mut().for_each(f);
    }

    /// Same channel layout as `other` (mono ↔ stereo), for mixing two branches.
    pub fn match_channels(&self, n: usize) -> Audio {
        if n == self.channels() {
            return self.clone();
        }
        let m = self.mix_mono();
        Audio {
            rate: self.rate,
            ch: vec![m; n.max(1)],
        }
    }

    /// Linear-interpolated resample to `rate` (used to bring a second branch to the main one's
    /// rate and for playback; ops never change the rate of the main chain).
    pub fn resampled(&self, rate: u32) -> Audio {
        if rate == self.rate || self.frames() == 0 {
            return self.clone();
        }
        let ratio = self.rate as f64 / rate as f64;
        let n = (self.frames() as f64 / ratio).round() as usize;
        Audio {
            rate,
            ch: self
                .ch
                .par_iter()
                .map(|c| (0..n).map(|i| lerp_at(c, i as f64 * ratio)).collect())
                .collect(),
        }
    }
}

/// Linear interpolation into a sample array (clamped at the ends).
#[inline]
pub fn lerp_at(c: &[f32], pos: f64) -> f32 {
    if c.is_empty() {
        return 0.0;
    }
    let i = pos.floor();
    let t = (pos - i) as f32;
    let i = i as isize;
    let a = c[i.clamp(0, c.len() as isize - 1) as usize];
    let b = c[(i + 1).clamp(0, c.len() as isize - 1) as usize];
    a + (b - a) * t
}

/// 4-point Hermite interpolation (better than linear for pitch shifting). Zero outside.
#[inline]
pub fn hermite_at(c: &[f32], pos: f64) -> f32 {
    let i = pos.floor() as isize;
    let t = (pos - pos.floor()) as f32;
    let g = |k: isize| -> f32 {
        if k < 0 || k >= c.len() as isize {
            0.0
        } else {
            c[k as usize]
        }
    };
    let (xm1, x0, x1, x2) = (g(i - 1), g(i), g(i + 1), g(i + 2));
    let c0 = x0;
    let c1 = 0.5 * (x1 - xm1);
    let c2 = xm1 - 2.5 * x0 + 2.0 * x1 - 0.5 * x2;
    let c3 = 0.5 * (x2 - xm1) + 1.5 * (x0 - x1);
    ((c3 * t + c2) * t + c1) * t + c0
}

pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

pub fn gain_to_db(g: f32) -> f32 {
    if g <= 1e-9 { -180.0 } else { 20.0 * g.log10() }
}

/// Deterministic hash → 0..1 (splitmix64). Seeded randomness so a recipe always renders the
/// same sound.
#[inline]
pub fn hash01(seed: u32, a: u32, b: u32) -> f32 {
    let mut z = (seed as u64) << 40 ^ (a as u64) << 20 ^ (b as u64) ^ 0x9E37_79B9_7F4A_7C15;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z >> 40) as f32 / (1u64 << 24) as f32
}

/// Small seeded PRNG (xorshift) for long random sequences (noise, grains).
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u32) -> Self {
        Self(((seed as u64) << 32 | 0x2545_F491) ^ 0x9E37_79B9_7F4A_7C15)
    }
    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    /// 0..1
    #[inline]
    pub fn f(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }
    /// -1..1
    #[inline]
    pub fn bi(&mut self) -> f32 {
        self.f() * 2.0 - 1.0
    }
}

/// Format a time in seconds as m:ss.mmm (or s.mmm under a minute).
pub fn fmt_time(s: f64) -> String {
    let s = s.max(0.0);
    let m = (s / 60.0).floor() as u64;
    let rest = s - m as f64 * 60.0;
    if m > 0 {
        format!("{m}:{rest:06.3}")
    } else {
        format!("{rest:.3}s")
    }
}

/// Short duration label for tiles: "0.25s", "6.5s", "3:12".
pub fn fmt_dur(s: f64) -> String {
    if s < 10.0 {
        format!("{s:.2}s")
    } else if s < 60.0 {
        format!("{s:.1}s")
    } else {
        let m = (s / 60.0).floor() as u64;
        format!("{m}:{:02}", (s - m as f64 * 60.0).round() as u64)
    }
}
