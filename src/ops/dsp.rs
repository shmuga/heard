//! DSP building blocks: RBJ-cookbook biquads, windows / FFT plans, envelope helpers and
//! ITU-R BS.1770 loudness.

use crate::audio::Audio;
use rustfft::num_complex::Complex32;
use std::f32::consts::PI;
use std::sync::Arc;

// ------------------------------------------------------------------ biquad

#[derive(Clone, Copy, Debug)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

#[derive(Clone, Copy, Debug)]
pub enum Kind {
    LowPass,
    HighPass,
    BandPass,
    Notch,
    Peak(f32),
    LowShelf(f32),
    HighShelf(f32),
}

impl Biquad {
    /// RBJ Audio-EQ-Cookbook coefficients. `freq` is clamped below Nyquist.
    pub fn new(kind: Kind, rate: u32, freq: f32, q: f32) -> Self {
        let fs = rate.max(1) as f32;
        let f = freq.clamp(1.0, fs * 0.49);
        let w = 2.0 * PI * f / fs;
        let (sw, cw) = w.sin_cos();
        let q = q.max(0.05);
        let alpha = sw / (2.0 * q);
        let (b0, b1, b2, a0, a1, a2) = match kind {
            Kind::LowPass => (
                (1.0 - cw) / 2.0,
                1.0 - cw,
                (1.0 - cw) / 2.0,
                1.0 + alpha,
                -2.0 * cw,
                1.0 - alpha,
            ),
            Kind::HighPass => (
                (1.0 + cw) / 2.0,
                -(1.0 + cw),
                (1.0 + cw) / 2.0,
                1.0 + alpha,
                -2.0 * cw,
                1.0 - alpha,
            ),
            Kind::BandPass => (alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * cw, 1.0 - alpha),
            Kind::Notch => (1.0, -2.0 * cw, 1.0, 1.0 + alpha, -2.0 * cw, 1.0 - alpha),
            Kind::Peak(db) => {
                let a = 10f32.powf(db / 40.0);
                (
                    1.0 + alpha * a,
                    -2.0 * cw,
                    1.0 - alpha * a,
                    1.0 + alpha / a,
                    -2.0 * cw,
                    1.0 - alpha / a,
                )
            }
            Kind::LowShelf(db) => {
                let a = 10f32.powf(db / 40.0);
                let s = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) - (a - 1.0) * cw + s),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cw),
                    a * ((a + 1.0) - (a - 1.0) * cw - s),
                    (a + 1.0) + (a - 1.0) * cw + s,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cw),
                    (a + 1.0) + (a - 1.0) * cw - s,
                )
            }
            Kind::HighShelf(db) => {
                let a = 10f32.powf(db / 40.0);
                let s = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) + (a - 1.0) * cw + s),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cw),
                    a * ((a + 1.0) + (a - 1.0) * cw - s),
                    (a + 1.0) - (a - 1.0) * cw + s,
                    2.0 * ((a - 1.0) - (a + 1.0) * cw),
                    (a + 1.0) - (a - 1.0) * cw - s,
                )
            }
        };
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    #[inline]
    pub fn tick(&mut self, x: f32) -> f32 {
        // Transposed direct form II.
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }

    pub fn run(&mut self, s: &mut [f32]) {
        for v in s {
            *v = self.tick(*v);
        }
    }
}

/// Butterworth section Qs for 2nd / 4th / 8th order (12 / 24 / 48 dB per octave).
pub fn butterworth_qs(slope: usize) -> &'static [f32] {
    match slope {
        0 => &[std::f32::consts::FRAC_1_SQRT_2],
        1 => &[0.541_196_1, 1.306_563],
        _ => &[0.509_795_6, 0.601_344_9, 0.899_976_2, 2.562_915_4],
    }
}

/// Run a cascade of biquads over every channel.
pub fn filter(a: &mut Audio, make: impl Fn() -> Vec<Biquad> + Sync + Send) {
    a.map_channels(|c| {
        for mut b in make() {
            b.run(c);
        }
    });
}

// ------------------------------------------------------------------ windows / FFT

pub fn hann(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / n as f32).cos())
        .collect()
}

pub struct Fft {
    pub n: usize,
    pub fwd: Arc<dyn rustfft::Fft<f32>>,
    pub inv: Arc<dyn rustfft::Fft<f32>>,
    pub win: Vec<f32>,
}

impl Fft {
    pub fn new(n: usize) -> Self {
        let mut p = rustfft::FftPlanner::<f32>::new();
        Self {
            n,
            fwd: p.plan_fft_forward(n),
            inv: p.plan_fft_inverse(n),
            win: hann(n),
        }
    }

    /// Windowed forward transform of `x[start..start+n]` (zero padded outside).
    pub fn analyze(&self, x: &[f32], start: isize, out: &mut [Complex32]) {
        for (k, o) in out.iter_mut().enumerate() {
            let i = start + k as isize;
            let v = if i >= 0 && (i as usize) < x.len() {
                x[i as usize]
            } else {
                0.0
            };
            *o = Complex32::new(v * self.win[k], 0.0);
        }
        self.fwd.process(out);
    }

    /// Inverse transform, windowed again, added into `y` at `start`; window² sum into `norm`.
    pub fn synth_add(&self, spec: &mut [Complex32], start: isize, y: &mut [f32], norm: &mut [f32]) {
        self.inv.process(spec);
        let scale = 1.0 / self.n as f32;
        for k in 0..self.n {
            let i = start + k as isize;
            if i >= 0 && (i as usize) < y.len() {
                let w = self.win[k];
                y[i as usize] += spec[k].re * scale * w;
                norm[i as usize] += w * w;
            }
        }
    }
}

/// Make a spectrum Hermitian-symmetric from its first half (bins 0..=n/2) so the inverse is
/// real.
pub fn mirror(spec: &mut [Complex32]) {
    let n = spec.len();
    spec[0].im = 0.0;
    spec[n / 2].im = 0.0;
    for k in 1..n / 2 {
        spec[n - k] = spec[k].conj();
    }
}

/// FFT size for a given rate: ~46 ms windows (2048 at 44.1 kHz).
pub fn fft_size(rate: u32, base_ms: f32) -> usize {
    let target = (rate as f32 * base_ms / 1000.0).max(64.0);
    target.log2().round().exp2() as usize
}

/// Divide by the window² sum (overlap-add normalisation).
pub fn normalize_ola(y: &mut [f32], norm: &[f32]) {
    let peak = norm.iter().cloned().fold(0.0f32, f32::max);
    let floor = (peak * 0.1).max(1e-6);
    for (v, n) in y.iter_mut().zip(norm) {
        *v /= n.max(floor);
    }
}

// ------------------------------------------------------------------ loudness

/// Integrated loudness in LUFS (ITU-R BS.1770-4: K-weighting, 400 ms blocks with 75 %
/// overlap, absolute gate −70 LUFS, relative gate −10 LU). Short sounds (< 400 ms) use the
/// whole sound as one block.
pub fn lufs(a: &Audio) -> f32 {
    let n = a.frames();
    if n == 0 {
        return -70.0;
    }
    let weighted: Vec<Vec<f32>> =
        a.ch.iter()
            .map(|c| {
                let mut s = c.clone();
                let mut st1 = Biquad::new(Kind::HighShelf(4.0), a.rate, 1681.97, 0.7072);
                let mut st2 = Biquad::new(Kind::HighPass, a.rate, 38.13, 0.5003);
                for v in s.iter_mut() {
                    *v = st2.tick(st1.tick(*v));
                }
                s
            })
            .collect();
    let block = ((a.rate as f64 * 0.4) as usize).max(1);
    let step = (block / 4).max(1);
    let mut blocks: Vec<f64> = vec![];
    let mut start = 0;
    loop {
        let end = (start + block).min(n);
        let ms: f64 = weighted
            .iter()
            .map(|c| {
                c[start..end]
                    .iter()
                    .map(|v| (*v as f64).powi(2))
                    .sum::<f64>()
            })
            .sum::<f64>()
            / (end - start).max(1) as f64;
        blocks.push(ms);
        if end >= n {
            break;
        }
        start += step;
    }
    let to_l = |ms: f64| -0.691 + 10.0 * ms.max(1e-12).log10();
    let abs: Vec<f64> = blocks.into_iter().filter(|m| to_l(*m) > -70.0).collect();
    if abs.is_empty() {
        return -70.0;
    }
    let mean = abs.iter().sum::<f64>() / abs.len() as f64;
    let rel = to_l(mean) - 10.0;
    let gated: Vec<f64> = abs.into_iter().filter(|m| to_l(*m) > rel).collect();
    let m = if gated.is_empty() {
        mean
    } else {
        gated.iter().sum::<f64>() / gated.len() as f64
    };
    to_l(m) as f32
}

/// One-pole smoothing coefficient for a time constant in ms.
#[inline]
pub fn coef(ms: f32, rate: u32) -> f32 {
    if ms <= 0.0 {
        0.0
    } else {
        (-1.0 / (ms * 0.001 * rate as f32)).exp()
    }
}

/// Peak envelope over all channels (linked), with attack / release smoothing.
pub fn linked_envelope(a: &Audio, attack_ms: f32, release_ms: f32) -> Vec<f32> {
    let n = a.frames();
    let (ca, cr) = (coef(attack_ms, a.rate), coef(release_ms, a.rate));
    let mut env = vec![0.0; n];
    let mut e = 0.0f32;
    for i in 0..n {
        let x = a.ch.iter().fold(0.0f32, |m, c| m.max(c[i].abs()));
        let c = if x > e { ca } else { cr };
        e = x + c * (e - x);
        env[i] = e;
    }
    env
}

/// Equal-power fade curve value for t in 0..1 (0 → silent, 1 → full).
#[inline]
pub fn fade_curve(t: f32, curve: usize) -> f32 {
    let t = t.clamp(0.0, 1.0);
    match curve {
        0 => t,                                       // linear
        1 => (t * std::f32::consts::FRAC_PI_2).sin(), // equal power
        _ => t * t * t,                               // exponential-ish (slow start)
    }
}
