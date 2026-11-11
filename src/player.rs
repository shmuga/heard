//! Playback (cpal). One shared buffer, a playhead and an optional loop region; the audio
//! callback resamples to the device rate on the fly. Without an output device (or in tests)
//! the player is silent but keeps its state, so the UI behaves the same.

use crate::audio::{Audio, lerp_at};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct State {
    buf: Option<Arc<Audio>>,
    /// What is loaded (a path / preview key) so the UI knows what's playing.
    key: u64,
    /// Playhead in frames of `buf`.
    pos: f64,
    playing: bool,
    /// Play range in frames (end exclusive).
    start: f64,
    end: f64,
    looping: bool,
    device_rate: u32,
}

pub struct Player {
    state: Arc<Mutex<State>>,
    _stream: Option<cpal::Stream>,
    pub error: Option<String>,
}

impl Player {
    /// A player with no audio output (tests, headless).
    pub fn silent() -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                device_rate: 44100,
                ..Default::default()
            })),
            _stream: None,
            error: None,
        }
    }

    /// Open the default output device. Falls back to a silent player (with `error` set).
    pub fn open() -> Self {
        let mut p = Self::silent();
        match build_stream(p.state.clone()) {
            Ok((stream, rate)) => {
                p.state.lock().unwrap().device_rate = rate;
                p._stream = Some(stream);
            }
            Err(e) => {
                log::warn!("no audio output: {e:#}");
                p.error = Some(format!("{e:#}"));
            }
        }
        p
    }

    /// Start playing `buf` from `from` seconds, up to `to` (None = end), looping if asked.
    pub fn play(&self, buf: Arc<Audio>, key: u64, from: f64, to: Option<f64>, looping: bool) {
        let mut s = self.state.lock().unwrap();
        let rate = buf.rate as f64;
        let n = buf.frames() as f64;
        let start = (from * rate).clamp(0.0, n);
        let end = to.map(|t| (t * rate).clamp(start, n)).unwrap_or(n);
        s.start = if looping { start } else { 0.0 };
        s.end = if end - start < 64.0 { n } else { end };
        s.pos = start;
        s.buf = Some(buf);
        s.key = key;
        s.looping = looping;
        s.playing = true;
    }

    /// Replace the buffer (e.g. a fresh preview render) keeping the playhead.
    pub fn swap(&self, buf: Arc<Audio>, key: u64) {
        let mut s = self.state.lock().unwrap();
        if s.buf.is_none() {
            return;
        }
        let n = buf.frames() as f64;
        s.pos = s.pos.min(n);
        s.end = if s.looping { s.end.min(n) } else { n };
        s.start = s.start.min(s.end);
        s.buf = Some(buf);
        s.key = key;
    }

    pub fn stop(&self) {
        self.state.lock().unwrap().playing = false;
    }

    pub fn is_playing(&self) -> bool {
        self.state.lock().unwrap().playing
    }

    pub fn is_playing_key(&self, key: u64) -> bool {
        let s = self.state.lock().unwrap();
        s.playing && s.key == key
    }

    pub fn key(&self) -> u64 {
        self.state.lock().unwrap().key
    }

    /// Playhead in seconds of the loaded buffer.
    pub fn position(&self) -> f64 {
        let s = self.state.lock().unwrap();
        s.buf
            .as_ref()
            .map(|b| s.pos / b.rate.max(1) as f64)
            .unwrap_or(0.0)
    }

    /// Move the playhead (seconds); keeps playing if it was.
    pub fn seek(&self, t: f64) {
        let mut s = self.state.lock().unwrap();
        if let Some(b) = &s.buf {
            let n = b.frames() as f64;
            s.pos = (t * b.rate as f64).clamp(0.0, n);
        }
    }

    pub fn has_output(&self) -> bool {
        self._stream.is_some()
    }

    /// Advance a silent player's playhead by `dt` seconds (so the UI still shows movement
    /// without a device; tests use it too).
    pub fn tick_silent(&self, dt: f64) {
        if self._stream.is_some() {
            return;
        }
        let mut s = self.state.lock().unwrap();
        if !s.playing {
            return;
        }
        let Some(rate) = s.buf.as_ref().map(|b| b.rate as f64) else {
            return;
        };
        s.pos += dt * rate;
        if s.pos >= s.end {
            if s.looping {
                s.pos = s.start;
            } else {
                s.playing = false;
            }
        }
    }
}

fn build_stream(state: Arc<Mutex<State>>) -> anyhow::Result<(cpal::Stream, u32)> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    let host = cpal::default_host();
    let dev = host
        .default_output_device()
        .ok_or_else(|| anyhow::anyhow!("no output device"))?;
    let cfg = dev.default_output_config()?;
    let rate = cfg.sample_rate().0;
    let channels = cfg.channels() as usize;
    let config: cpal::StreamConfig = cfg.clone().into();
    let err = |e| log::warn!("audio stream error: {e}");
    let stream = match cfg.sample_format() {
        cpal::SampleFormat::F32 => dev.build_output_stream(
            &config,
            move |out: &mut [f32], _| fill(&state, out, channels, rate, |v| v),
            err,
            None,
        )?,
        cpal::SampleFormat::I16 => dev.build_output_stream(
            &config,
            move |out: &mut [i16], _| {
                fill(&state, out, channels, rate, |v| {
                    (v.clamp(-1.0, 1.0) * 32767.0) as i16
                })
            },
            err,
            None,
        )?,
        cpal::SampleFormat::I32 => dev.build_output_stream(
            &config,
            move |out: &mut [i32], _| {
                fill(&state, out, channels, rate, |v| {
                    (v.clamp(-1.0, 1.0) as f64 * i32::MAX as f64) as i32
                })
            },
            err,
            None,
        )?,
        cpal::SampleFormat::U16 => dev.build_output_stream(
            &config,
            move |out: &mut [u16], _| {
                fill(&state, out, channels, rate, |v| {
                    ((v.clamp(-1.0, 1.0) * 0.5 + 0.5) * 65535.0) as u16
                })
            },
            err,
            None,
        )?,
        f => anyhow::bail!("unsupported output sample format {f:?}"),
    };
    stream.play()?;
    Ok((stream, rate))
}

fn fill<T: Copy + Default>(
    state: &Mutex<State>,
    out: &mut [T],
    channels: usize,
    device_rate: u32,
    conv: impl Fn(f32) -> T,
) {
    let silence = conv(0.0);
    // Never block the audio thread on the UI.
    let Ok(mut s) = state.try_lock() else {
        out.iter_mut().for_each(|v| *v = silence);
        return;
    };
    let (Some(buf), true) = (s.buf.clone(), s.playing) else {
        out.iter_mut().for_each(|v| *v = silence);
        return;
    };
    let step = buf.rate as f64 / device_rate.max(1) as f64;
    let nch = buf.channels().max(1);
    for frame in out.chunks_mut(channels.max(1)) {
        if !s.playing {
            frame.iter_mut().for_each(|v| *v = silence);
            continue;
        }
        for (c, o) in frame.iter_mut().enumerate() {
            let src = &buf.ch[c.min(nch - 1)];
            *o = conv(lerp_at(src, s.pos).clamp(-1.0, 1.0));
        }
        s.pos += step;
        if s.pos >= s.end {
            if s.looping {
                s.pos = s.start;
            } else {
                s.playing = false;
            }
        }
    }
}
