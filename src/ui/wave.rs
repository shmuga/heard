//! Waveform drawing: a horizontal time view (zoom / pan), a min/max peak pyramid so long
//! takes draw instantly at any zoom, the time ruler and the spectrogram image.

use super::theme;
use crate::audio::Audio;
use egui::{Color32, Painter, Rect, Stroke, pos2};
use rayon::prelude::*;

/// Visible time range. `span == 0` means "fit the whole sound".
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TimeView {
    pub start: f64,
    pub span: f64,
}

pub const MIN_SPAN: f64 = 0.002;

impl TimeView {
    pub fn fit(&mut self) {
        self.start = 0.0;
        self.span = 0.0;
    }

    pub fn is_fit(&self) -> bool {
        self.span <= 0.0
    }

    /// (t0, t1) for a sound of `len` seconds.
    pub fn range(&self, len: f64) -> (f64, f64) {
        if self.span <= 0.0 || self.span >= len {
            return (0.0, len.max(MIN_SPAN));
        }
        let s = self.start.clamp(0.0, (len - self.span).max(0.0));
        (s, s + self.span)
    }

    pub fn clamp(&mut self, len: f64) {
        if self.span > 0.0 && self.span >= len {
            self.fit();
        } else if self.span > 0.0 {
            self.start = self.start.clamp(0.0, (len - self.span).max(0.0));
        }
    }

    /// Zoom by `factor` (> 1 = in) around time `anchor`.
    pub fn zoom_at(&mut self, factor: f64, anchor: f64, len: f64) {
        let (t0, t1) = self.range(len);
        let span = ((t1 - t0) / factor).clamp(MIN_SPAN, len.max(MIN_SPAN));
        let u = if t1 > t0 {
            (anchor - t0) / (t1 - t0)
        } else {
            0.5
        };
        self.start = anchor - u * span;
        self.span = span;
        self.clamp(len);
    }

    pub fn zoom_in(&mut self, len: f64) {
        let (a, b) = self.range(len);
        self.zoom_at(1.5, (a + b) / 2.0, len);
    }

    pub fn zoom_out(&mut self, len: f64) {
        let (a, b) = self.range(len);
        self.zoom_at(1.0 / 1.5, (a + b) / 2.0, len);
    }

    pub fn show(&mut self, a: f64, b: f64, len: f64) {
        let pad = (b - a) * 0.05;
        self.start = (a - pad).max(0.0);
        self.span = (b - a + 2.0 * pad).max(MIN_SPAN);
        self.clamp(len);
    }

    pub fn pan(&mut self, dt: f64, len: f64) {
        if self.is_fit() {
            return;
        }
        self.start += dt;
        self.clamp(len);
    }

    /// Zoom factor relative to fit (for the status bar).
    pub fn factor(&self, len: f64) -> f64 {
        let (a, b) = self.range(len);
        len / (b - a).max(MIN_SPAN)
    }
}

/// Screen x ↔ time inside `rect` for the range (t0, t1).
#[derive(Clone, Copy, Debug)]
pub struct Map {
    pub rect: Rect,
    pub t0: f64,
    pub t1: f64,
}

impl Map {
    pub fn x(&self, t: f64) -> f32 {
        self.rect.min.x + ((t - self.t0) / (self.t1 - self.t0).max(1e-9)) as f32 * self.rect.width()
    }
    pub fn t(&self, x: f32) -> f64 {
        self.t0 + ((x - self.rect.min.x) / self.rect.width().max(1.0)) as f64 * (self.t1 - self.t0)
    }
    pub fn secs_per_px(&self) -> f64 {
        (self.t1 - self.t0) / self.rect.width().max(1.0) as f64
    }
}

/// Per channel, (min, max) per block.
pub type Level = Vec<Vec<(f32, f32)>>;

/// Min / max pyramid per channel.
pub struct WavePeaks {
    pub rate: u32,
    pub frames: usize,
    /// (block size, per channel [(min, max)])
    pub levels: Vec<(usize, Level)>,
}

impl WavePeaks {
    pub fn of(a: &Audio) -> Self {
        let mut levels = vec![];
        let base = 32usize;
        let first: Vec<Vec<(f32, f32)>> =
            a.ch.par_iter()
                .map(|c| {
                    c.chunks(base)
                        .map(|b| {
                            b.iter()
                                .fold((f32::MAX, f32::MIN), |(lo, hi), v| (lo.min(*v), hi.max(*v)))
                        })
                        .collect()
                })
                .collect();
        levels.push((base, first));
        while levels.last().is_some_and(|(_, l)| l[0].len() > 256) {
            let (bs, prev) = levels.last().unwrap();
            let next: Vec<Vec<(f32, f32)>> = prev
                .iter()
                .map(|c| {
                    c.chunks(4)
                        .map(|b| {
                            b.iter().fold((f32::MAX, f32::MIN), |(lo, hi), (a, b)| {
                                (lo.min(*a), hi.max(*b))
                            })
                        })
                        .collect()
                })
                .collect();
            let bs = bs * 4;
            levels.push((bs, next));
        }
        Self {
            rate: a.rate,
            frames: a.frames(),
            levels,
        }
    }

    /// min / max of channel `c` over frames `f0..f1` (f1 > f0), using blocks no larger than
    /// the range.
    fn range(&self, c: usize, f0: usize, f1: usize) -> Option<(f32, f32)> {
        let span = f1.saturating_sub(f0).max(1);
        let (bs, lv) = self
            .levels
            .iter()
            .rev()
            .find(|(bs, _)| *bs <= span)
            .or(self.levels.first())?;
        let ch = lv.get(c)?;
        let (b0, b1) = (f0 / bs, f1.div_ceil(*bs).min(ch.len()));
        if b0 >= b1 {
            return None;
        }
        Some(
            ch[b0..b1]
                .iter()
                .fold((f32::MAX, f32::MIN), |(lo, hi), (a, b)| {
                    (lo.min(*a), hi.max(*b))
                }),
        )
    }
}

/// Colours for one waveform drawing.
#[derive(Clone, Copy)]
pub struct WaveLook {
    pub fill: Color32,
    pub line: Color32,
    pub center: Color32,
}

impl WaveLook {
    pub fn normal() -> Self {
        Self {
            fill: theme::WAVE,
            line: theme::WAVE,
            center: theme::LINE,
        }
    }
    pub fn dim() -> Self {
        Self {
            fill: theme::WAVE_DIM,
            line: theme::WAVE_DIM,
            center: theme::LINE,
        }
    }
}

/// Draw every channel stacked in `rect` for the time range in `map`. Zoomed in far enough,
/// the samples are drawn as a line.
pub fn draw(p: &Painter, map: Map, audio: &Audio, peaks: &WavePeaks, look: WaveLook) {
    let nch = audio.channels().max(1);
    let lane_h = map.rect.height() / nch as f32;
    let rate = audio.rate as f64;
    let spp = map.secs_per_px() * rate; // frames per pixel
    for c in 0..nch {
        let lane = Rect::from_min_max(
            pos2(map.rect.min.x, map.rect.min.y + lane_h * c as f32),
            pos2(map.rect.max.x, map.rect.min.y + lane_h * (c + 1) as f32),
        );
        let mid = lane.center().y;
        let amp = lane.height() * 0.46;
        p.hline(lane.x_range(), mid, Stroke::new(1.0, look.center));
        if c > 0 {
            p.hline(
                lane.x_range(),
                lane.min.y,
                Stroke::new(1.0, theme::LIST_SEP),
            );
        }
        let y = |v: f32| mid - v.clamp(-1.0, 1.0) * amp;
        if spp < 2.0 {
            // Sample view: a polyline through the samples.
            let ch = &audio.ch[c];
            let f0 = (map.t0 * rate).floor().max(0.0) as usize;
            let f1 = ((map.t1 * rate).ceil() as usize + 1).min(ch.len());
            if f1 > f0 {
                let pts: Vec<egui::Pos2> = (f0..f1)
                    .map(|i| pos2(map.x(i as f64 / rate), y(ch[i])))
                    .collect();
                p.add(egui::Shape::line(pts.clone(), Stroke::new(1.5, look.line)));
                if spp < 0.25 {
                    for q in pts {
                        p.circle_filled(q, 2.0, look.line);
                    }
                }
            }
            continue;
        }
        let x0 = map.rect.min.x.floor() as i32;
        let x1 = map.rect.max.x.ceil() as i32;
        let mut mesh = egui::Mesh::default();
        for x in x0..x1 {
            let ta = map.t(x as f32);
            let tb = map.t(x as f32 + 1.0);
            if tb <= 0.0 {
                continue;
            }
            let fa = (ta.max(0.0) * rate) as usize;
            let fb = ((tb * rate) as usize).min(peaks.frames);
            if fb <= fa {
                continue;
            }
            if let Some((lo, hi)) = peaks.range(c, fa, fb) {
                let (top, bot) = (y(hi), y(lo).max(y(hi) + 1.0));
                mesh.add_colored_rect(
                    Rect::from_min_max(pos2(x as f32, top), pos2(x as f32 + 1.0, bot)),
                    look.fill,
                );
            }
        }
        p.add(egui::Shape::mesh(mesh));
    }
}

/// Time ruler with "nice" tick spacing.
pub fn ruler(p: &Painter, rect: Rect, map: Map) {
    p.rect_filled(rect, 0.0, theme::TOOLBAR);
    p.hline(
        rect.x_range(),
        rect.max.y - 0.5,
        Stroke::new(1.0, theme::LINE),
    );
    let spp = map.secs_per_px();
    let target = spp * 90.0;
    let steps = [
        0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0,
        30.0, 60.0, 120.0, 300.0, 600.0,
    ];
    let step = steps
        .iter()
        .copied()
        .find(|s| *s >= target)
        .unwrap_or(600.0);
    let minor = step / 5.0;
    let mut t = (map.t0 / minor).floor() * minor;
    while t <= map.t1 {
        let x = map.x(t);
        let major = ((t / step).round() * step - t).abs() < minor * 0.01;
        let h = if major { 8.0 } else { 4.0 };
        p.vline(
            x,
            (rect.max.y - h)..=rect.max.y,
            Stroke::new(1.0, theme::TEXT_DIM),
        );
        if major && t >= -1e-9 {
            p.text(
                pos2(x + 3.0, rect.min.y + 2.0),
                egui::Align2::LEFT_TOP,
                time_label(t.max(0.0), step),
                egui::FontId::monospace(10.0),
                theme::TEXT_DIM,
            );
        }
        t += minor;
    }
}

fn time_label(t: f64, step: f64) -> String {
    let m = (t / 60.0).floor() as u64;
    let s = t - m as f64 * 60.0;
    let dec = if step < 0.01 {
        3
    } else if step < 0.1 {
        2
    } else if step < 1.0 {
        1
    } else {
        0
    };
    if m > 0 {
        format!("{m}:{:0w$.dec$}", s, w = if dec > 0 { dec + 3 } else { 2 })
    } else {
        format!("{s:.dec$}s")
    }
}

// ------------------------------------------------------------------ spectrogram

pub const SPEC_ROWS: usize = 256;

/// Log-frequency spectrogram of the mono mix as an image, `cols` columns spanning the sound.
pub fn spectrogram(a: &Audio, cols: usize) -> egui::ColorImage {
    use crate::ops::dsp::Fft;
    use rustfft::num_complex::Complex32;
    let cols = cols.clamp(16, 4096);
    let rows = SPEC_ROWS;
    let m = a.mix_mono();
    // Short sounds get shorter windows (a 46 ms window would smear a 0.3 s hit).
    let win_ms = (a.seconds() as f32 * 1000.0 / 24.0).clamp(6.0, 46.0);
    let n = crate::ops::dsp::fft_size(a.rate, win_ms).max(256);
    let bins = n / 2 + 1;
    let nyq = a.rate as f32 / 2.0;
    let (fmin, fmax) = (30.0f32, nyq);
    // Row r (0 = top) → bin range.
    let edges: Vec<f32> = (0..=rows)
        .map(|r| {
            let u = 1.0 - r as f32 / rows as f32;
            fmin * (fmax / fmin).powf(u) / nyq * (bins - 1) as f32
        })
        .collect();
    let frames = m.len();
    let mags: Vec<Vec<f32>> = (0..cols)
        .into_par_iter()
        .map_init(
            || (Fft::new(n), vec![Complex32::new(0.0, 0.0); n]),
            |(fft, buf), k| {
                let center = ((k as f64 + 0.5) / cols as f64 * frames as f64) as isize;
                fft.analyze(&m, center - (n / 2) as isize, buf);
                (0..rows)
                    .map(|r| {
                        let (hi, lo) = (edges[r], edges[r + 1]);
                        let (b0, b1) = (
                            lo.floor() as usize,
                            (hi.ceil() as usize).max(lo as usize + 1),
                        );
                        let mut mx = 0.0f32;
                        for b in b0..b1.min(bins) {
                            mx = mx.max(buf[b].norm());
                        }
                        20.0 * (mx / (n as f32 * 0.25) + 1e-9).log10()
                    })
                    .collect()
            },
        )
        .collect();
    let top = mags
        .iter()
        .flat_map(|c| c.iter())
        .cloned()
        .fold(-200.0f32, f32::max);
    let floor = top - 90.0;
    let mut px = vec![Color32::BLACK; cols * rows];
    for (k, col) in mags.iter().enumerate() {
        for (r, db) in col.iter().enumerate() {
            let t = ((db - floor) / (top - floor)).clamp(0.0, 1.0);
            px[r * cols + k] = colormap(t);
        }
    }
    egui::ColorImage::new([cols, rows], px)
}

/// Dark slate → blue → amber → near-white (fits the grey patcher theme).
pub fn colormap(t: f32) -> Color32 {
    let stops: [(f32, [f32; 3]); 5] = [
        (0.0, [38.0, 40.0, 44.0]),
        (0.35, [44.0, 78.0, 140.0]),
        (0.6, [120.0, 110.0, 150.0]),
        (0.8, [226.0, 170.0, 60.0]),
        (1.0, [250.0, 244.0, 220.0]),
    ];
    let t = t.clamp(0.0, 1.0);
    for w in stops.windows(2) {
        let ((a, ca), (b, cb)) = (w[0], w[1]);
        if t <= b {
            let u = (t - a) / (b - a);
            let c = |i: usize| (ca[i] + (cb[i] - ca[i]) * u) as u8;
            return Color32::from_rgb(c(0), c(1), c(2));
        }
    }
    Color32::from_rgb(250, 244, 220)
}
