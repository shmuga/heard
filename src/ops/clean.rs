//! Cleanup tools — the usual field-recording chain: trim, rumble high-pass, hum notches,
//! spectral noise reduction (learned profile, Audacity / noisereduce-style gating), de-click,
//! gate, compression, fades and loudness normalisation.

use super::dsp::{self, Biquad, Fft, Kind};
use crate::audio::{Audio, db_to_gain, gain_to_db};
use rayon::prelude::*;
use rustfft::num_complex::Complex32;

/// Short anti-click ramp at cut points (ms).
const EDGE_MS: f32 = 2.0;

pub fn trim(a: &Audio, start: f32, end: f32) -> Audio {
    let s = a.secs_to_frame(start as f64);
    let e = if end <= 0.0 {
        a.frames()
    } else {
        a.secs_to_frame(end as f64)
    };
    a.slice(s, e.max(s))
}

/// Remove leading / trailing audio below `threshold_db`, keeping `pad_ms` around it.
pub fn auto_trim(a: &Audio, threshold_db: f32, pad_ms: f32) -> Audio {
    let thr = db_to_gain(threshold_db);
    let n = a.frames();
    let loud = |i: usize| a.ch.iter().any(|c| c[i].abs() >= thr);
    let Some(first) = (0..n).find(|&i| loud(i)) else {
        return a.clone();
    };
    let last = (0..n).rev().find(|&i| loud(i)).unwrap_or(n - 1);
    let pad = (pad_ms.max(0.0) * 0.001 * a.rate as f32) as usize;
    let mut out = a.slice(first.saturating_sub(pad), (last + pad + 1).min(n));
    edge_fades(&mut out, EDGE_MS, EDGE_MS);
    out
}

pub fn gain(mut a: Audio, db: f32) -> Audio {
    a.gain(db_to_gain(db));
    a
}

pub const NORM_MODES: &[&str] = &["Peak", "Loudness (LUFS)", "RMS"];

pub fn normalize(mut a: Audio, mode: usize, target_db: f32) -> Audio {
    let cur = match mode {
        0 => gain_to_db(a.peak()),
        1 => dsp::lufs(&a),
        _ => gain_to_db(a.rms()),
    };
    if cur <= -120.0 || !cur.is_finite() {
        return a;
    }
    let mut g = db_to_gain(target_db - cur);
    // Loudness / RMS targets never push peaks over 0 dBFS.
    if mode != 0 {
        let peak = a.peak() * g;
        if peak > 0.999 {
            g *= 0.999 / peak;
        }
    }
    a.gain(g);
    a
}

pub const FADE_CURVES: &[&str] = &["Linear", "Equal power", "Exponential"];

pub fn fade(mut a: Audio, in_ms: f32, out_ms: f32, curve: usize) -> Audio {
    let n = a.frames();
    let fi = ((in_ms.max(0.0) * 0.001 * a.rate as f32) as usize).min(n);
    let fo = ((out_ms.max(0.0) * 0.001 * a.rate as f32) as usize).min(n);
    a.map_channels(|c| {
        for i in 0..fi {
            c[i] *= dsp::fade_curve(i as f32 / fi as f32, curve);
        }
        for i in 0..fo {
            c[n - 1 - i] *= dsp::fade_curve(i as f32 / fo as f32, curve);
        }
    });
    a
}

/// Tiny linear ramps so cuts don't click.
pub fn edge_fades(a: &mut Audio, in_ms: f32, out_ms: f32) {
    let n = a.frames();
    let fi = ((in_ms * 0.001 * a.rate as f32) as usize).min(n / 2);
    let fo = ((out_ms * 0.001 * a.rate as f32) as usize).min(n / 2);
    a.map_channels(|c| {
        for i in 0..fi {
            c[i] *= i as f32 / fi as f32;
        }
        for i in 0..fo {
            c[n - 1 - i] *= i as f32 / fo as f32;
        }
    });
}

pub const SLOPES: &[&str] = &["12 dB/oct", "24 dB/oct", "48 dB/oct"];

/// High-pass (rumble, wind, handling noise). `freq` 0 = off.
pub fn high_pass(mut a: Audio, freq: f32, slope: usize) -> Audio {
    if freq < 1.0 {
        return a;
    }
    let rate = a.rate;
    dsp::filter(&mut a, || {
        dsp::butterworth_qs(slope)
            .iter()
            .map(|q| Biquad::new(Kind::HighPass, rate, freq, *q))
            .collect()
    });
    a
}

/// Low-pass (hiss, harshness). Off at or above Nyquist.
pub fn low_pass(mut a: Audio, freq: f32, slope: usize) -> Audio {
    if freq >= a.rate as f32 * 0.49 {
        return a;
    }
    let rate = a.rate;
    dsp::filter(&mut a, || {
        dsp::butterworth_qs(slope)
            .iter()
            .map(|q| Biquad::new(Kind::LowPass, rate, freq, *q))
            .collect()
    });
    a
}

#[allow(clippy::too_many_arguments)]
pub fn eq(
    mut a: Audio,
    low_db: f32,
    low_hz: f32,
    mid_db: f32,
    mid_hz: f32,
    mid_q: f32,
    high_db: f32,
    high_hz: f32,
) -> Audio {
    let rate = a.rate;
    dsp::filter(&mut a, || {
        let mut v = vec![];
        if low_db != 0.0 {
            v.push(Biquad::new(Kind::LowShelf(low_db), rate, low_hz, 0.707));
        }
        if mid_db != 0.0 {
            v.push(Biquad::new(Kind::Peak(mid_db), rate, mid_hz, mid_q));
        }
        if high_db != 0.0 {
            v.push(Biquad::new(Kind::HighShelf(high_db), rate, high_hz, 0.707));
        }
        v
    });
    a
}

pub const HUM_BASES: &[&str] = &["50 Hz (EU / UK)", "60 Hz (US)"];

/// Mains hum: narrow notches at the base frequency and its harmonics.
pub fn hum(mut a: Audio, base: usize, harmonics: i32, q: f32) -> Audio {
    let f0 = if base == 0 { 50.0 } else { 60.0 };
    let rate = a.rate;
    dsp::filter(&mut a, || {
        (1..=harmonics.max(1))
            .map(|k| f0 * k as f32)
            .filter(|f| *f < rate as f32 * 0.45)
            // Twice through each notch: deeper, still narrow.
            .flat_map(|f| {
                [
                    Biquad::new(Kind::Notch, rate, f, q),
                    Biquad::new(Kind::Notch, rate, f, q),
                ]
            })
            .collect()
    });
    a
}

/// Spectral-gating noise reduction.
///
/// A noise profile (per-bin mean and spread of the magnitude in dB) is learned from
/// `profile` = [start, end] seconds of the input, or — when that's empty — from the quietest
/// 10 % of the sound. Bins below `mean + sensitivity · spread` are attenuated by `amount_db`;
/// the mask is smoothed over `smooth` (0..1 → a few bins / frames) so it doesn't warble
/// ("musical noise"). Processed in blocks, so long takes don't need gigabytes.
pub fn denoise(
    a: &Audio,
    amount_db: f32,
    sensitivity: f32,
    smooth: f32,
    profile: [f32; 2],
) -> Audio {
    if amount_db <= 0.0 || a.frames() < 64 {
        return a.clone();
    }
    let n = dsp::fft_size(a.rate, 46.0);
    let hop = n / 4;
    let floor = db_to_gain(-amount_db.min(80.0));
    // Narrow in frequency (a steady tone's main lobe is ~5 bins and must stay fully open),
    // wider in time (that's where musical noise flickers).
    let sf = (smooth.clamp(0.0, 1.0) * 3.0).round() as usize; // freq smoothing radius (bins)
    let st = (smooth.clamp(0.0, 1.0) * 4.0).round() as usize; // time smoothing radius (frames)
    let region = if profile[1] > profile[0] + 0.02 {
        Some((
            a.secs_to_frame(profile[0] as f64),
            a.secs_to_frame(profile[1] as f64),
        ))
    } else {
        None
    };
    let ch: Vec<Vec<f32>> =
        a.ch.par_iter()
            .map(|x| denoise_channel(x, n, hop, floor, sensitivity, sf, st, region))
            .collect();
    Audio::from_channels(a.rate, ch)
}

#[allow(clippy::too_many_arguments)]
fn denoise_channel(
    x: &[f32],
    n: usize,
    hop: usize,
    floor: f32,
    sensitivity: f32,
    sf: usize,
    st: usize,
    region: Option<(usize, usize)>,
) -> Vec<f32> {
    let fft = Fft::new(n);
    let bins = n / 2 + 1;
    let len = x.len();
    // Frame k starts at k*hop - n/2 (centred frames, so the edges are covered).
    let frames = len.div_ceil(hop) + 1;
    let start = |k: usize| k as isize * hop as isize - (n / 2) as isize;
    let mut buf = vec![Complex32::new(0.0, 0.0); n];

    // ---- noise profile frames
    let profile_frames: Vec<usize> = match region {
        Some((a, b)) => {
            let v: Vec<usize> = (0..frames)
                .filter(|&k| {
                    let s = start(k) + (n / 2) as isize;
                    s >= a as isize && s < b as isize
                })
                .collect();
            if v.is_empty() {
                vec![(a / hop).min(frames - 1)]
            } else {
                v
            }
        }
        None => {
            // Quietest 10 % of frames by energy.
            let mut e: Vec<(f32, usize)> = (0..frames)
                .map(|k| {
                    let s = start(k);
                    let mut sum = 0.0f32;
                    for i in 0..n {
                        let j = s + i as isize;
                        if j >= 0 && (j as usize) < len {
                            sum += x[j as usize] * x[j as usize];
                        }
                    }
                    (sum, k)
                })
                .collect();
            e.sort_by(|a, b| a.0.total_cmp(&b.0));
            let keep = (frames / 10).max(4).min(frames);
            e.into_iter().take(keep).map(|(_, k)| k).collect()
        }
    };
    let mut mean = vec![0.0f32; bins];
    let mut sq = vec![0.0f32; bins];
    for &k in &profile_frames {
        fft.analyze(x, start(k), &mut buf);
        for b in 0..bins {
            let d = 20.0 * (buf[b].norm() + 1e-9).log10();
            mean[b] += d;
            sq[b] += d * d;
        }
    }
    let m = profile_frames.len().max(1) as f32;
    let thresh: Vec<f32> = (0..bins)
        .map(|b| {
            let mu = mean[b] / m;
            let var = (sq[b] / m - mu * mu).max(0.0);
            mu + sensitivity * var.sqrt()
        })
        .collect();

    // ---- gate, in blocks of frames (with a margin for time smoothing)
    let mut y = vec![0.0f32; len];
    let mut norm = vec![0.0f32; len];
    const BLOCK: usize = 128;
    let mut b0 = 0;
    while b0 < frames {
        let b1 = (b0 + BLOCK).min(frames);
        let (m0, m1) = (b0.saturating_sub(st), (b1 + st).min(frames));
        let count = m1 - m0;
        let mut specs: Vec<Vec<Complex32>> = Vec::with_capacity(count);
        let mut mask = vec![0.0f32; count * bins];
        for (r, k) in (m0..m1).enumerate() {
            fft.analyze(x, start(k), &mut buf);
            for b in 0..bins {
                let d = 20.0 * (buf[b].norm() + 1e-9).log10();
                mask[r * bins + b] = if d > thresh[b] { 1.0 } else { 0.0 };
            }
            specs.push(buf.clone());
        }
        // Separable box smoothing of the mask.
        if sf > 0 {
            let mut tmp = vec![0.0f32; bins];
            for r in 0..count {
                let row = &mut mask[r * bins..(r + 1) * bins];
                for b in 0..bins {
                    let (lo, hi) = (b.saturating_sub(sf), (b + sf + 1).min(bins));
                    tmp[b] = row[lo..hi].iter().sum::<f32>() / (hi - lo) as f32;
                }
                row.copy_from_slice(&tmp);
            }
        }
        let smoothed: Vec<f32> = if st > 0 {
            let mut out = vec![0.0f32; count * bins];
            for r in 0..count {
                let (lo, hi) = (r.saturating_sub(st), (r + st + 1).min(count));
                for b in 0..bins {
                    let mut s = 0.0;
                    for rr in lo..hi {
                        s += mask[rr * bins + b];
                    }
                    out[r * bins + b] = s / (hi - lo) as f32;
                }
            }
            out
        } else {
            mask
        };
        for k in b0..b1 {
            let r = k - m0;
            let spec = &mut specs[r];
            for b in 0..bins {
                let mk = smoothed[r * bins + b];
                spec[b] *= floor + (1.0 - floor) * mk;
            }
            dsp::mirror(spec);
            fft.synth_add(spec, start(k), &mut y, &mut norm);
        }
        b0 = b1;
    }
    dsp::normalize_ola(&mut y, &norm);
    y
}

/// Impulsive clicks / crackle: samples whose second difference is far above the local
/// level are re-drawn by cubic interpolation across the click.
pub fn declick(mut a: Audio, sensitivity: f32, width_ms: f32) -> Audio {
    let rate = a.rate;
    let width = ((width_ms.max(0.05) * 0.001 * rate as f32) as usize).max(1);
    a.map_channels(|c| {
        let n = c.len();
        if n < 16 {
            return;
        }
        let d: Vec<f32> = (0..n)
            .map(|i| {
                let p = if i > 0 { c[i - 1] } else { c[i] };
                let q = if i + 1 < n { c[i + 1] } else { c[i] };
                (c[i] - 0.5 * (p + q)).abs()
            })
            .collect();
        // Robust local scale: median of |d| per 1024-sample block.
        const B: usize = 1024;
        let scales: Vec<f32> = d
            .chunks(B)
            .map(|ch| {
                let mut v = ch.to_vec();
                let mid = v.len() / 2;
                v.select_nth_unstable_by(mid, |a, b| a.total_cmp(b));
                v[mid].max(1e-5)
            })
            .collect();
        let thr = sensitivity.max(1.0);
        let mut i = 0;
        while i < n {
            if d[i] > thr * 6.0 * scales[i / B] {
                let a0 = i.saturating_sub(width);
                let mut b0 = (i + width).min(n - 1);
                // Extend over adjacent outliers.
                while b0 + 1 < n && d[b0] > thr * 6.0 * scales[b0 / B] && b0 - a0 < width * 8 {
                    b0 += 1;
                }
                repair(c, a0, b0);
                i = b0 + 1;
            } else {
                i += 1;
            }
        }
    });
    a
}

/// Replace `c[a..=b]` with a Hermite curve between the good samples around it.
fn repair(c: &mut [f32], a: usize, b: usize) {
    let n = c.len();
    if a < 2 || b + 2 >= n || b <= a {
        return;
    }
    let (p0, p1) = (c[a - 1], c[b + 1]);
    let m0 = c[a - 1] - c[a - 2];
    let m1 = c[b + 2] - c[b + 1];
    let len = (b - a + 2) as f32;
    for i in a..=b {
        let t = (i - a + 1) as f32 / len;
        let (t2, t3) = (t * t, t * t * t);
        c[i] = (2.0 * t3 - 3.0 * t2 + 1.0) * p0
            + (t3 - 2.0 * t2 + t) * m0 * len
            + (-2.0 * t3 + 3.0 * t2) * p1
            + (t3 - t2) * m1 * len;
    }
}

/// Noise gate (linked): below `threshold_db` the level drops to `floor_db`.
pub fn gate(
    mut a: Audio,
    threshold_db: f32,
    attack_ms: f32,
    hold_ms: f32,
    release_ms: f32,
    floor_db: f32,
) -> Audio {
    let env = dsp::linked_envelope(&a, 0.5, 30.0);
    let thr = db_to_gain(threshold_db);
    let floor = db_to_gain(floor_db);
    let (ca, cr) = (dsp::coef(attack_ms, a.rate), dsp::coef(release_ms, a.rate));
    let hold = (hold_ms.max(0.0) * 0.001 * a.rate as f32) as usize;
    let mut g = vec![0.0f32; env.len()];
    let (mut cur, mut held) = (floor, 0usize);
    for i in 0..env.len() {
        let open = env[i] >= thr;
        if open {
            held = hold;
        } else {
            held = held.saturating_sub(1);
        }
        let target = if open || held > 0 { 1.0 } else { floor };
        let c = if target > cur { ca } else { cr };
        cur = target + c * (cur - target);
        g[i] = cur;
    }
    a.map_channels(|c| c.iter_mut().zip(&g).for_each(|(v, g)| *v *= g));
    a
}

/// Feed-forward compressor, linked stereo, 6 dB soft knee.
pub fn compress(
    mut a: Audio,
    threshold_db: f32,
    ratio: f32,
    attack_ms: f32,
    release_ms: f32,
    makeup_db: f32,
) -> Audio {
    let env = dsp::linked_envelope(&a, attack_ms, release_ms);
    let ratio = ratio.max(1.0);
    let knee = 6.0;
    let makeup = makeup_db;
    let g: Vec<f32> = env
        .par_iter()
        .map(|e| {
            let x = gain_to_db(*e);
            let over = x - threshold_db;
            let red = if over <= -knee / 2.0 {
                0.0
            } else if over >= knee / 2.0 {
                over * (1.0 - 1.0 / ratio)
            } else {
                let t = over + knee / 2.0;
                (1.0 - 1.0 / ratio) * t * t / (2.0 * knee)
            };
            db_to_gain(makeup - red)
        })
        .collect();
    a.map_channels(|c| c.iter_mut().zip(&g).for_each(|(v, g)| *v *= g));
    a
}

pub const CHANNEL_MODES: &[&str] = &[
    "Keep",
    "Mono (mix)",
    "Left only",
    "Right only",
    "Swap L / R",
    "Stereo width",
];

pub fn channels(a: Audio, mode: usize, width: f32) -> Audio {
    let stereo = a.channels() >= 2;
    match mode {
        1 => Audio::mono(a.rate, a.mix_mono()),
        2 => Audio::mono(a.rate, a.ch[0].clone()),
        3 if stereo => Audio::mono(a.rate, a.ch[1].clone()),
        4 if stereo => {
            let mut a = a;
            a.ch.swap(0, 1);
            a
        }
        5 if stereo => {
            let mut a = a;
            let (l, r) = a.ch.split_at_mut(1);
            for (x, y) in l[0].iter_mut().zip(r[0].iter_mut()) {
                let m = (*x + *y) * 0.5;
                let s = (*x - *y) * 0.5 * width;
                *x = m + s;
                *y = m - s;
            }
            a
        }
        _ => a,
    }
}

/// Seamless loop: the tail is crossfaded (equal power) into the head and removed, so the
/// sound repeats without a click — for drones and room tones.
pub fn make_loop(a: &Audio, crossfade_ms: f32) -> Audio {
    let n = a.frames();
    let l = ((crossfade_ms.max(1.0) * 0.001 * a.rate as f32) as usize).min(n / 2);
    if l < 2 {
        return a.clone();
    }
    let ch =
        a.ch.iter()
            .map(|c| {
                let mut out = c[..n - l].to_vec();
                for i in 0..l {
                    let t = i as f32 / l as f32;
                    let (fi, fo) = (
                        (t * std::f32::consts::FRAC_PI_2).sin(),
                        (t * std::f32::consts::FRAC_PI_2).cos(),
                    );
                    out[i] = c[i] * fi + c[n - l + i] * fo;
                }
                out
            })
            .collect();
    Audio::from_channels(a.rate, ch)
}

/// Onset times (seconds) for slicing a take of separate hits: a 5 ms RMS envelope rising
/// above `threshold_db` after at least `min_gap_s` below it. Each marker sits 10 ms before
/// its hit; the first hit gets none (it starts the first slice).
pub fn onsets(a: &Audio, threshold_db: f32, min_gap_s: f32) -> Vec<f64> {
    let rate = a.rate as f64;
    let hop = ((rate * 0.005) as usize).max(1);
    let thr = db_to_gain(threshold_db);
    let m = a.mix_mono();
    let env: Vec<f32> = m
        .chunks(hop)
        .map(|c| (c.iter().map(|v| v * v).sum::<f32>() / c.len() as f32).sqrt())
        .collect();
    let gap = ((min_gap_s as f64 * rate) as usize / hop).max(1);
    let mut out = vec![];
    let mut quiet = gap; // frames spent below the threshold
    let mut first = true;
    for (k, e) in env.iter().enumerate() {
        if *e >= thr {
            if quiet >= gap {
                let t = (k * hop) as f64 / rate - 0.01;
                if !first && t > 0.02 {
                    out.push(t);
                }
                first = false;
            }
            quiet = 0;
        } else {
            quiet += 1;
        }
    }
    out
}
