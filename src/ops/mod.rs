//! Node operations. Times are in seconds / milliseconds and frequencies in Hz, so a recipe
//! means the same thing at any sample rate; random choices are seeded.

pub mod clean;
pub mod dsp;
pub mod fx;

use crate::audio::Audio;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// UI-facing parameter description (mutable view into an op's fields).
pub enum Param<'a> {
    F {
        label: &'static str,
        v: &'a mut f32,
        min: f32,
        max: f32,
    },
    I {
        label: &'static str,
        v: &'a mut i32,
        min: i32,
        max: i32,
    },
    B {
        label: &'static str,
        v: &'a mut bool,
    },
    C {
        label: &'static str,
        v: &'a mut usize,
        options: &'static [&'static str],
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Io,
    Clean,
    Experimental,
}

/// The cut: start / end in seconds of the source (`end` 0 = to the end).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Trim {
    pub start: f32,
    pub end: f32,
}

impl Trim {
    pub fn is_identity(&self) -> bool {
        self.start <= 0.0 && self.end <= 0.0
    }

    /// End time for a source of `len` seconds.
    pub fn end_or(&self, len: f32) -> f32 {
        if self.end <= 0.0 {
            len
        } else {
            self.end.min(len)
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op")]
pub enum Op {
    Source,
    Output,
    Trim(Trim),
    AutoTrim {
        threshold: f32,
        pad: f32,
    },
    Gain {
        db: f32,
    },
    Normalize {
        mode: usize,
        target: f32,
    },
    Fade {
        fade_in: f32,
        fade_out: f32,
        curve: usize,
    },
    HighPass {
        freq: f32,
        slope: usize,
    },
    LowPass {
        freq: f32,
        slope: usize,
    },
    Eq {
        low_db: f32,
        low_hz: f32,
        mid_db: f32,
        mid_hz: f32,
        mid_q: f32,
        high_db: f32,
        high_hz: f32,
    },
    Hum {
        base: usize,
        harmonics: i32,
        q: f32,
    },
    NoiseReduction {
        amount: f32,
        sensitivity: f32,
        smooth: f32,
        /// Learned noise region [start, end] in seconds of this node's input; empty = auto.
        #[serde(default)]
        profile: [f32; 2],
    },
    DeClick {
        sensitivity: f32,
        width: f32,
    },
    Gate {
        threshold: f32,
        attack: f32,
        hold: f32,
        release: f32,
        floor: f32,
    },
    Compressor {
        threshold: f32,
        ratio: f32,
        attack: f32,
        release: f32,
        makeup: f32,
    },
    Channels {
        mode: usize,
        width: f32,
    },
    Loop {
        crossfade: f32,
    },
    // ---- experimental ----
    Reverse,
    Varispeed {
        semitones: f32,
        cents: f32,
    },
    Stretch {
        factor: f32,
        window: f32,
        seed: i32,
    },
    Freeze {
        position: f32,
        length: f32,
        smear: f32,
        seed: i32,
    },
    PitchShift {
        semitones: f32,
        window: f32,
    },
    Granular {
        grain: f32,
        density: f32,
        spread: f32,
        pitch: f32,
        reverse: f32,
        length: f32,
        seed: i32,
    },
    Stutter {
        slice: f32,
        chance: f32,
        repeats: i32,
        seed: i32,
    },
    BitCrush {
        bits: i32,
        downsample: i32,
    },
    Tape {
        wow: f32,
        flutter: f32,
        saturation: f32,
        hiss: f32,
        tone: f32,
        seed: i32,
    },
    RingMod {
        freq: f32,
        mix: f32,
    },
    Resonator {
        freq: f32,
        feedback: f32,
        damping: f32,
        mix: f32,
    },
    Echo {
        time: f32,
        feedback: f32,
        mix: f32,
        ping_pong: bool,
    },
    Reverb {
        size: f32,
        damping: f32,
        mix: f32,
        predelay: f32,
        tail: f32,
    },
    Drive {
        drive: f32,
        tone: f32,
        mix: f32,
    },
    Mix {
        mode: usize,
        level: f32,
        offset: f32,
    },
    /// A follows B's loudness: gated (A only where B sounds) or ducked (A dips under B).
    Sidechain {
        mode: usize,
        threshold: f32,
        depth: f32,
        release: f32,
    },
    /// Three outputs: low / mid / high bands that add back up to the input exactly.
    Split {
        low_hz: f32,
        high_hz: f32,
    },
}

macro_rules! f {
    ($l:expr, $v:expr, $a:expr, $b:expr) => {
        Param::F {
            label: $l,
            v: $v,
            min: $a,
            max: $b,
        }
    };
}
macro_rules! i {
    ($l:expr, $v:expr, $a:expr, $b:expr) => {
        Param::I {
            label: $l,
            v: $v,
            min: $a,
            max: $b,
        }
    };
}
macro_rules! c {
    ($l:expr, $v:expr, $o:expr) => {
        Param::C {
            label: $l,
            v: $v,
            options: $o,
        }
    };
}

impl Op {
    /// Everything that can be added from the "+" menus, with defaults.
    pub fn catalog() -> Vec<Op> {
        vec![
            Op::Trim(Trim::default()),
            Op::AutoTrim {
                threshold: -50.0,
                pad: 15.0,
            },
            Op::HighPass {
                freq: 80.0,
                slope: 1,
            },
            Op::LowPass {
                freq: 12000.0,
                slope: 1,
            },
            Op::Hum {
                base: 0,
                harmonics: 6,
                q: 30.0,
            },
            Op::NoiseReduction {
                amount: 12.0,
                sensitivity: 1.5,
                smooth: 0.5,
                profile: [0.0, 0.0],
            },
            Op::DeClick {
                sensitivity: 3.0,
                width: 0.6,
            },
            Op::Eq {
                low_db: 0.0,
                low_hz: 150.0,
                mid_db: 0.0,
                mid_hz: 1500.0,
                mid_q: 1.0,
                high_db: 0.0,
                high_hz: 6000.0,
            },
            Op::Gate {
                threshold: -45.0,
                attack: 2.0,
                hold: 40.0,
                release: 120.0,
                floor: -60.0,
            },
            Op::Compressor {
                threshold: -18.0,
                ratio: 3.0,
                attack: 10.0,
                release: 150.0,
                makeup: 0.0,
            },
            Op::Gain { db: 0.0 },
            Op::Normalize {
                mode: 0,
                target: -1.0,
            },
            Op::Fade {
                fade_in: 5.0,
                fade_out: 30.0,
                curve: 1,
            },
            Op::Channels {
                mode: 1,
                width: 1.0,
            },
            Op::Loop { crossfade: 250.0 },
            Op::Reverse,
            Op::Varispeed {
                semitones: -12.0,
                cents: 0.0,
            },
            Op::Stretch {
                factor: 8.0,
                window: 0.25,
                seed: 1,
            },
            Op::Freeze {
                position: 0.5,
                length: 8.0,
                smear: 0.2,
                seed: 1,
            },
            Op::PitchShift {
                semitones: 7.0,
                window: 60.0,
            },
            Op::Granular {
                grain: 80.0,
                density: 40.0,
                spread: 0.2,
                pitch: 2.0,
                reverse: 0.2,
                length: 2.0,
                seed: 1,
            },
            Op::Stutter {
                slice: 90.0,
                chance: 0.25,
                repeats: 3,
                seed: 1,
            },
            Op::BitCrush {
                bits: 8,
                downsample: 4,
            },
            Op::Tape {
                wow: 0.4,
                flutter: 0.3,
                saturation: 0.3,
                hiss: 0.15,
                tone: 0.4,
                seed: 1,
            },
            Op::RingMod {
                freq: 220.0,
                mix: 0.7,
            },
            Op::Resonator {
                freq: 110.0,
                feedback: 0.9,
                damping: 0.3,
                mix: 0.6,
            },
            Op::Echo {
                time: 320.0,
                feedback: 0.45,
                mix: 0.4,
                ping_pong: true,
            },
            Op::Reverb {
                size: 0.7,
                damping: 0.5,
                mix: 0.35,
                predelay: 20.0,
                tail: 3.0,
            },
            Op::Drive {
                drive: 12.0,
                tone: 0.6,
                mix: 1.0,
            },
            Op::Mix {
                mode: 0,
                level: 0.7,
                offset: 0.0,
            },
            Op::Sidechain {
                mode: 0,
                threshold: -30.0,
                depth: 1.0,
                release: 120.0,
            },
            Op::Split {
                low_hz: 250.0,
                high_hz: 4000.0,
            },
        ]
    }

    pub fn name(&self) -> &'static str {
        match self {
            Op::Source => "Source",
            Op::Output => "Output",
            Op::Trim(_) => "Trim",
            Op::AutoTrim { .. } => "Auto Trim",
            Op::Gain { .. } => "Gain",
            Op::Normalize { .. } => "Normalize",
            Op::Fade { .. } => "Fade",
            Op::HighPass { .. } => "High-pass",
            Op::LowPass { .. } => "Low-pass",
            Op::Eq { .. } => "EQ",
            Op::Hum { .. } => "Hum Removal",
            Op::NoiseReduction { .. } => "Noise Reduction",
            Op::DeClick { .. } => "De-click",
            Op::Gate { .. } => "Gate",
            Op::Compressor { .. } => "Compressor",
            Op::Channels { .. } => "Channels",
            Op::Loop { .. } => "Loop",
            Op::Reverse => "Reverse",
            Op::Varispeed { .. } => "Varispeed",
            Op::Stretch { .. } => "Paulstretch",
            Op::Freeze { .. } => "Freeze",
            Op::PitchShift { .. } => "Pitch Shift",
            Op::Granular { .. } => "Granular",
            Op::Stutter { .. } => "Stutter",
            Op::BitCrush { .. } => "Bit Crush",
            Op::Tape { .. } => "Tape",
            Op::RingMod { .. } => "Ring Mod",
            Op::Resonator { .. } => "Resonator",
            Op::Echo { .. } => "Echo",
            Op::Reverb { .. } => "Reverb",
            Op::Drive { .. } => "Drive",
            Op::Mix { .. } => "Mix",
            Op::Sidechain { .. } => "Sidechain",
            Op::Split { .. } => "Split",
        }
    }

    /// One-line explanation (hover text in the add menu and on cards).
    pub fn about(&self) -> &'static str {
        match self {
            Op::Source => "The original recording",
            Op::Output => "What gets rendered to .edit.wav",
            Op::Trim(_) => "Cut the start and end (drag the handles on the waveform)",
            Op::AutoTrim { .. } => "Strip silence before and after the sound",
            Op::Gain { .. } => "Turn it up or down",
            Op::Normalize { .. } => "Bring the peak or loudness (LUFS) to a target",
            Op::Fade { .. } => "Fade in / out",
            Op::HighPass { .. } => "Remove rumble, wind and handling noise (80–120 Hz)",
            Op::LowPass { .. } => "Tame hiss and harshness",
            Op::Eq { .. } => "Low shelf, mid bell, high shelf",
            Op::Hum { .. } => "Notch out mains hum (50/60 Hz) and its harmonics",
            Op::NoiseReduction { .. } => {
                "Spectral gating: learn the noise from a quiet selection, then reduce it"
            }
            Op::DeClick { .. } => "Repair clicks, pops and crackle",
            Op::Gate { .. } => "Silence everything below a threshold",
            Op::Compressor { .. } => "Even out the level",
            Op::Channels { .. } => "Mono mix, pick a side, swap, stereo width",
            Op::Loop { .. } => "Crossfade the end into the start for a seamless loop",
            Op::Reverse => "Play backwards",
            Op::Varispeed { .. } => "Tape speed: pitch and length change together",
            Op::Stretch { .. } => "Extreme time stretch into a smeared texture",
            Op::Freeze { .. } => "Hold one moment of the spectrum indefinitely",
            Op::PitchShift { .. } => "Change pitch, keep the length",
            Op::Granular { .. } => "Scatter tiny grains into a cloud",
            Op::Stutter { .. } => "Randomly repeat slices",
            Op::BitCrush { .. } => "Lower bit depth and sample rate",
            Op::Tape { .. } => "Wow, flutter, saturation and hiss",
            Op::RingMod { .. } => "Multiply with a sine: metallic, bell-like",
            Op::Resonator { .. } => "Make noise ring at a pitch",
            Op::Echo { .. } => "Delay with feedback",
            Op::Reverb { .. } => "Room / hall reverb",
            Op::Drive { .. } => "Saturate and distort",
            Op::Mix { .. } => "Combine two branches (patch view)",
            Op::Sidechain { .. } => {
                "A follows B's loudness: gate (A only where B sounds) or duck (patch view)"
            }
            Op::Split { .. } => {
                "Low / mid / high bands on three outputs; Mix them back to treat each differently (patch view)"
            }
        }
    }

    pub fn category(&self) -> Category {
        match self {
            Op::Source | Op::Output => Category::Io,
            Op::Trim(_)
            | Op::AutoTrim { .. }
            | Op::Gain { .. }
            | Op::Normalize { .. }
            | Op::Fade { .. }
            | Op::HighPass { .. }
            | Op::LowPass { .. }
            | Op::Eq { .. }
            | Op::Hum { .. }
            | Op::NoiseReduction { .. }
            | Op::DeClick { .. }
            | Op::Gate { .. }
            | Op::Compressor { .. }
            | Op::Channels { .. }
            | Op::Loop { .. } => Category::Clean,
            _ => Category::Experimental,
        }
    }

    pub fn inputs(&self) -> usize {
        match self {
            Op::Source => 0,
            Op::Mix { .. } | Op::Sidechain { .. } => 2,
            _ => 1,
        }
    }

    pub fn outputs(&self) -> usize {
        match self {
            Op::Output => 0,
            Op::Split { .. } => 3,
            _ => 1,
        }
    }

    /// Pin labels for the patch view.
    pub fn input_label(&self, i: usize) -> &'static str {
        match (self, i) {
            (Op::Mix { .. } | Op::Sidechain { .. }, 0) => "A",
            (Op::Mix { .. }, _) => "B",
            (Op::Sidechain { .. }, _) => "key",
            _ => "in",
        }
    }

    pub fn output_label(&self, i: usize) -> &'static str {
        match (self, i) {
            (Op::Split { .. }, 0) => "low",
            (Op::Split { .. }, 1) => "mid",
            (Op::Split { .. }, _) => "high",
            _ => "",
        }
    }

    pub fn params(&mut self) -> Vec<Param<'_>> {
        match self {
            Op::Source | Op::Output | Op::Trim(_) | Op::Reverse => vec![],
            Op::AutoTrim { threshold, pad } => vec![
                f!("Threshold dB", threshold, -90.0, -10.0),
                f!("Padding ms", pad, 0.0, 500.0),
            ],
            Op::Gain { db } => vec![f!("Gain dB", db, -36.0, 24.0)],
            Op::Normalize { mode, target } => vec![
                c!("Mode", mode, clean::NORM_MODES),
                f!("Target dB / LUFS", target, -40.0, 0.0),
            ],
            Op::Fade {
                fade_in,
                fade_out,
                curve,
            } => vec![
                f!("Fade in ms", fade_in, 0.0, 5000.0),
                f!("Fade out ms", fade_out, 0.0, 5000.0),
                c!("Curve", curve, clean::FADE_CURVES),
            ],
            Op::HighPass { freq, slope } => vec![
                f!("Freq Hz (0 = off)", freq, 0.0, 1000.0),
                c!("Slope", slope, clean::SLOPES),
            ],
            Op::LowPass { freq, slope } => vec![
                f!("Freq Hz", freq, 500.0, 22000.0),
                c!("Slope", slope, clean::SLOPES),
            ],
            Op::Eq {
                low_db,
                low_hz,
                mid_db,
                mid_hz,
                mid_q,
                high_db,
                high_hz,
            } => vec![
                f!("Low dB", low_db, -18.0, 18.0),
                f!("Low Hz", low_hz, 30.0, 600.0),
                f!("Mid dB", mid_db, -18.0, 18.0),
                f!("Mid Hz", mid_hz, 150.0, 8000.0),
                f!("Mid Q", mid_q, 0.2, 10.0),
                f!("High dB", high_db, -18.0, 18.0),
                f!("High Hz", high_hz, 1500.0, 16000.0),
            ],
            Op::Hum { base, harmonics, q } => vec![
                c!("Mains", base, clean::HUM_BASES),
                i!("Harmonics", harmonics, 1, 16),
                f!("Narrowness Q", q, 5.0, 100.0),
            ],
            Op::NoiseReduction {
                amount,
                sensitivity,
                smooth,
                ..
            } => vec![
                f!("Reduction dB", amount, 0.0, 40.0),
                f!("Sensitivity", sensitivity, 0.0, 4.0),
                f!("Smoothing", smooth, 0.0, 1.0),
            ],
            Op::DeClick { sensitivity, width } => vec![
                f!("Sensitivity", sensitivity, 1.0, 10.0),
                f!("Click width ms", width, 0.05, 5.0),
            ],
            Op::Gate {
                threshold,
                attack,
                hold,
                release,
                floor,
            } => vec![
                f!("Threshold dB", threshold, -90.0, 0.0),
                f!("Attack ms", attack, 0.1, 100.0),
                f!("Hold ms", hold, 0.0, 1000.0),
                f!("Release ms", release, 5.0, 2000.0),
                f!("Floor dB", floor, -90.0, 0.0),
            ],
            Op::Compressor {
                threshold,
                ratio,
                attack,
                release,
                makeup,
            } => vec![
                f!("Threshold dB", threshold, -60.0, 0.0),
                f!("Ratio", ratio, 1.0, 20.0),
                f!("Attack ms", attack, 0.1, 200.0),
                f!("Release ms", release, 5.0, 2000.0),
                f!("Makeup dB", makeup, 0.0, 24.0),
            ],
            Op::Channels { mode, width } => vec![
                c!("Mode", mode, clean::CHANNEL_MODES),
                f!("Width", width, 0.0, 2.0),
            ],
            Op::Loop { crossfade } => vec![f!("Crossfade ms", crossfade, 5.0, 5000.0)],
            Op::Varispeed { semitones, cents } => vec![
                f!("Semitones", semitones, -36.0, 24.0),
                f!("Cents", cents, -100.0, 100.0),
            ],
            Op::Stretch {
                factor,
                window,
                seed,
            } => vec![
                f!("Stretch ×", factor, 1.0, 50.0),
                f!("Window s", window, 0.02, 1.0),
                i!("Seed", seed, 0, 9999),
            ],
            Op::Freeze {
                position,
                length,
                smear,
                seed,
            } => vec![
                f!("Position", position, 0.0, 1.0),
                f!("Length s", length, 0.5, 60.0),
                f!("Smear", smear, 0.0, 1.0),
                i!("Seed", seed, 0, 9999),
            ],
            Op::PitchShift { semitones, window } => vec![
                f!("Semitones", semitones, -24.0, 24.0),
                f!("Window ms", window, 10.0, 200.0),
            ],
            Op::Granular {
                grain,
                density,
                spread,
                pitch,
                reverse,
                length,
                seed,
            } => vec![
                f!("Grain ms", grain, 5.0, 500.0),
                f!("Density /s", density, 1.0, 200.0),
                f!("Spread", spread, 0.0, 1.0),
                f!("Pitch jitter st", pitch, 0.0, 12.0),
                f!("Reverse chance", reverse, 0.0, 1.0),
                f!("Length ×", length, 0.25, 8.0),
                i!("Seed", seed, 0, 9999),
            ],
            Op::Stutter {
                slice,
                chance,
                repeats,
                seed,
            } => vec![
                f!("Slice ms", slice, 10.0, 1000.0),
                f!("Chance", chance, 0.0, 1.0),
                i!("Repeats", repeats, 1, 16),
                i!("Seed", seed, 0, 9999),
            ],
            Op::BitCrush { bits, downsample } => vec![
                i!("Bits", bits, 1, 16),
                i!("Downsample ×", downsample, 1, 64),
            ],
            Op::Tape {
                wow,
                flutter,
                saturation,
                hiss,
                tone,
                seed,
            } => vec![
                f!("Wow", wow, 0.0, 1.0),
                f!("Flutter", flutter, 0.0, 1.0),
                f!("Saturation", saturation, 0.0, 1.0),
                f!("Hiss", hiss, 0.0, 1.0),
                f!("Age (dull)", tone, 0.0, 1.0),
                i!("Seed", seed, 0, 9999),
            ],
            Op::RingMod { freq, mix } => {
                vec![f!("Freq Hz", freq, 1.0, 4000.0), f!("Mix", mix, 0.0, 1.0)]
            }
            Op::Resonator {
                freq,
                feedback,
                damping,
                mix,
            } => vec![
                f!("Pitch Hz", freq, 30.0, 2000.0),
                f!("Feedback", feedback, 0.0, 0.99),
                f!("Damping", damping, 0.0, 0.95),
                f!("Mix", mix, 0.0, 1.0),
            ],
            Op::Echo {
                time,
                feedback,
                mix,
                ping_pong,
            } => vec![
                f!("Time ms", time, 10.0, 2000.0),
                f!("Feedback", feedback, 0.0, 0.95),
                f!("Mix", mix, 0.0, 1.0),
                Param::B {
                    label: "Ping-pong",
                    v: ping_pong,
                },
            ],
            Op::Reverb {
                size,
                damping,
                mix,
                predelay,
                tail,
            } => vec![
                f!("Size", size, 0.0, 1.0),
                f!("Damping", damping, 0.0, 1.0),
                f!("Mix", mix, 0.0, 1.0),
                f!("Pre-delay ms", predelay, 0.0, 200.0),
                f!("Tail s", tail, 0.0, 15.0),
            ],
            Op::Drive { drive, tone, mix } => vec![
                f!("Drive dB", drive, 0.0, 40.0),
                f!("Tone", tone, 0.0, 1.0),
                f!("Mix", mix, 0.0, 1.0),
            ],
            Op::Mix {
                mode,
                level,
                offset,
            } => vec![
                c!("Mode", mode, fx::MIX_MODES),
                f!("B level", level, 0.0, 1.0),
                f!("B offset s", offset, 0.0, 30.0),
            ],
            Op::Sidechain {
                mode,
                threshold,
                depth,
                release,
            } => vec![
                c!("Mode", mode, fx::SIDECHAIN_MODES),
                f!("Threshold dB", threshold, -70.0, 0.0),
                f!("Depth", depth, 0.0, 1.0),
                f!("Release ms", release, 5.0, 2000.0),
            ],
            Op::Split { low_hz, high_hz } => vec![
                f!("Low / mid Hz", low_hz, 40.0, 2000.0),
                f!("Mid / high Hz", high_hz, 1000.0, 16000.0),
            ],
        }
    }

    /// Experimental: dice roll every parameter (deterministic for a given roll number).
    /// Ranges that would make enormous files are kept modest.
    pub fn randomize(&mut self, roll: u32) {
        for (i, p) in self.params().into_iter().enumerate() {
            let r = crate::audio::hash01(roll, i as u32, 99);
            match p {
                Param::F { v, min, max, label } => {
                    let max = if label.contains('×') || label.starts_with("Length") {
                        min + (max - min).min(12.0) * 0.5
                    } else {
                        max
                    };
                    *v = min + (max - min) * r
                }
                Param::I { v, min, max, .. } => *v = min + ((max - min) as f32 * r).round() as i32,
                Param::B { v, .. } => *v = r > 0.5,
                Param::C { v, options, .. } => {
                    *v = ((options.len() as f32 * r) as usize).min(options.len() - 1)
                }
            }
        }
    }

    /// Leaves the sound exactly as it is (the neutral defaults of the starting chain).
    pub fn is_identity(&self) -> bool {
        match self {
            Op::Source | Op::Output => true,
            Op::Trim(t) => t.is_identity(),
            Op::HighPass { freq, .. } => *freq < 1.0,
            Op::NoiseReduction { amount, .. } => *amount <= 0.0,
            Op::Fade {
                fade_in, fade_out, ..
            } => *fade_in <= 0.0 && *fade_out <= 0.0,
            Op::Gain { db } => *db == 0.0,
            Op::Eq {
                low_db,
                mid_db,
                high_db,
                ..
            } => *low_db == 0.0 && *mid_db == 0.0 && *high_db == 0.0,
            Op::Channels { mode, .. } => *mode == 0,
            _ => false,
        }
    }

    /// Does this op change the length / timeline (so a waveform selection can't be mapped
    /// through it)?
    pub fn changes_time(&self) -> bool {
        matches!(
            self,
            Op::Trim(_)
                | Op::AutoTrim { .. }
                | Op::Loop { .. }
                | Op::Reverse
                | Op::Varispeed { .. }
                | Op::Stretch { .. }
                | Op::Freeze { .. }
                | Op::Granular { .. }
                | Op::Mix { .. }
                | Op::Sidechain { .. }
        )
    }

    /// Apply to the inputs, producing every output (one for most ops, three for Split).
    pub fn apply_outputs(&self, mut inputs: Vec<Option<Arc<Audio>>>) -> Option<Vec<Audio>> {
        if let Op::Split { low_hz, high_hz } = self {
            let src = inputs.get_mut(0)?.take()?;
            return Some(fx::split_bands(&src, *low_hz, *high_hz));
        }
        self.apply(inputs).map(|a| vec![a])
    }

    /// Apply to the inputs. `None` if a required input is missing. (Split: its low band.)
    pub fn apply(&self, mut inputs: Vec<Option<Arc<Audio>>>) -> Option<Audio> {
        let take = |a: Arc<Audio>| Arc::try_unwrap(a).unwrap_or_else(|a| (*a).clone());
        if let Op::Mix {
            mode,
            level,
            offset,
        } = self
        {
            let a = inputs.get_mut(0)?.take()?;
            return match inputs.get_mut(1).and_then(|b| b.take()) {
                Some(b) => Some(fx::mix(take(a), &b, *mode, *level, *offset)),
                None => Some(take(a)),
            };
        }
        if let Op::Sidechain {
            mode,
            threshold,
            depth,
            release,
        } = self
        {
            let a = inputs.get_mut(0)?.take()?;
            return match inputs.get_mut(1).and_then(|b| b.take()) {
                Some(k) => Some(fx::sidechain(
                    take(a),
                    &k,
                    *mode,
                    *threshold,
                    *depth,
                    *release,
                )),
                None => Some(take(a)),
            };
        }
        if let Op::Split { low_hz, high_hz } = self {
            let src = inputs.get_mut(0)?.take()?;
            return fx::split_bands(&src, *low_hz, *high_hz).into_iter().next();
        }
        let src = take(inputs.get_mut(0)?.take()?);
        Some(match self {
            Op::Source | Op::Output => src,
            Op::Trim(t) => clean::trim(&src, t.start, t.end),
            Op::AutoTrim { threshold, pad } => clean::auto_trim(&src, *threshold, *pad),
            Op::Gain { db } => clean::gain(src, *db),
            Op::Normalize { mode, target } => clean::normalize(src, *mode, *target),
            Op::Fade {
                fade_in,
                fade_out,
                curve,
            } => clean::fade(src, *fade_in, *fade_out, *curve),
            Op::HighPass { freq, slope } => clean::high_pass(src, *freq, *slope),
            Op::LowPass { freq, slope } => clean::low_pass(src, *freq, *slope),
            Op::Eq {
                low_db,
                low_hz,
                mid_db,
                mid_hz,
                mid_q,
                high_db,
                high_hz,
            } => clean::eq(
                src, *low_db, *low_hz, *mid_db, *mid_hz, *mid_q, *high_db, *high_hz,
            ),
            Op::Hum { base, harmonics, q } => clean::hum(src, *base, *harmonics, *q),
            Op::NoiseReduction {
                amount,
                sensitivity,
                smooth,
                profile,
            } => clean::denoise(&src, *amount, *sensitivity, *smooth, *profile),
            Op::DeClick { sensitivity, width } => clean::declick(src, *sensitivity, *width),
            Op::Gate {
                threshold,
                attack,
                hold,
                release,
                floor,
            } => clean::gate(src, *threshold, *attack, *hold, *release, *floor),
            Op::Compressor {
                threshold,
                ratio,
                attack,
                release,
                makeup,
            } => clean::compress(src, *threshold, *ratio, *attack, *release, *makeup),
            Op::Channels { mode, width } => clean::channels(src, *mode, *width),
            Op::Loop { crossfade } => clean::make_loop(&src, *crossfade),
            Op::Reverse => fx::reverse(src),
            Op::Varispeed { semitones, cents } => fx::varispeed(&src, *semitones, *cents),
            Op::Stretch {
                factor,
                window,
                seed,
            } => fx::stretch(&src, *factor, *window, *seed as u32),
            Op::Freeze {
                position,
                length,
                smear,
                seed,
            } => fx::freeze(&src, *position, *length, *smear, *seed as u32),
            Op::PitchShift { semitones, window } => fx::pitch_shift(&src, *semitones, *window),
            Op::Granular {
                grain,
                density,
                spread,
                pitch,
                reverse,
                length,
                seed,
            } => fx::granular(
                &src,
                *grain,
                *density,
                *spread,
                *pitch,
                *reverse,
                *length,
                *seed as u32,
            ),
            Op::Stutter {
                slice,
                chance,
                repeats,
                seed,
            } => fx::stutter(&src, *slice, *chance, *repeats, *seed as u32),
            Op::BitCrush { bits, downsample } => fx::bit_crush(src, *bits, *downsample),
            Op::Tape {
                wow,
                flutter,
                saturation,
                hiss,
                tone,
                seed,
            } => fx::tape(
                &src,
                *wow,
                *flutter,
                *saturation,
                *hiss,
                *tone,
                *seed as u32,
            ),
            Op::RingMod { freq, mix } => fx::ring_mod(src, *freq, *mix),
            Op::Resonator {
                freq,
                feedback,
                damping,
                mix,
            } => fx::resonator(&src, *freq, *feedback, *damping, *mix),
            Op::Echo {
                time,
                feedback,
                mix,
                ping_pong,
            } => fx::echo(&src, *time, *feedback, *mix, *ping_pong),
            Op::Reverb {
                size,
                damping,
                mix,
                predelay,
                tail,
            } => fx::reverb(&src, *size, *damping, *mix, *predelay, *tail),
            Op::Drive { drive, tone, mix } => fx::drive(src, *drive, *tone, *mix),
            Op::Mix { .. } | Op::Sidechain { .. } | Op::Split { .. } => unreachable!(),
        })
    }
}
