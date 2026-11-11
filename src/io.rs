//! Decoding (symphonia: WAV/BWF, AIFF, FLAC, MP3, AAC/ALAC in M4A, Ogg Vorbis), WAV writing
//! (hound), waveform overviews for the library (cached on disk) and BWF recording dates.

use crate::audio::Audio;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// File extensions Heard imports.
pub const AUDIO_EXTS: &[&str] = &[
    "wav", "wave", "bwf", "aif", "aiff", "aifc", "flac", "mp3", "m4a", "mp4", "aac", "caf", "ogg",
    "oga",
];

pub fn is_audio(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIO_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
        && !p
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("._"))
}

/// Facts about a source file (shown in the editor / list, used to match the export format).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SoundInfo {
    pub rate: u32,
    pub channels: usize,
    /// Bits per sample of the source (0 = unknown / lossy).
    pub bits: u32,
    pub float: bool,
    pub codec: String,
    pub seconds: f64,
    pub peak_db: f32,
    /// Recording time from a BWF `bext` chunk (unix seconds, local time), if present.
    #[serde(default)]
    pub recorded: Option<i64>,
}

impl SoundInfo {
    /// "44.1k · 24-bit · stereo" style label.
    pub fn format_label(&self) -> String {
        let rate = if self.rate.is_multiple_of(1000) {
            format!("{}k", self.rate / 1000)
        } else {
            format!("{:.1}k", self.rate as f32 / 1000.0)
        };
        let depth = match (self.float, self.bits) {
            (true, b) => format!("{}f", if b == 0 { 32 } else { b }),
            (false, 0) => self.codec.to_uppercase(),
            (false, b) => format!("{b}-bit"),
        };
        let ch = match self.channels {
            1 => "mono".to_string(),
            2 => "stereo".to_string(),
            n => format!("{n} ch"),
        };
        format!("{rate} · {depth} · {ch}")
    }
}

/// Decode a whole file to f32.
pub fn load(path: &Path) -> Result<(Audio, SoundInfo)> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
    use symphonia::core::errors::Error;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(e) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(&e.to_ascii_lowercase());
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .with_context(|| format!("unsupported or damaged audio: {}", path.display()))?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .context("no audio track")?
        .clone();
    let cp = &track.codec_params;
    let mut decoder = symphonia::default::get_codecs()
        .make(cp, &DecoderOptions::default())
        .context("no decoder for this codec")?;
    let codec = symphonia::default::get_codecs()
        .get_codec(cp.codec)
        .map(|d| d.short_name.to_string())
        .unwrap_or_default();
    let mut rate = cp.sample_rate.unwrap_or(0);
    let mut channels = cp.channels.map(|c| c.count()).unwrap_or(0);
    let bits = cp.bits_per_sample.unwrap_or(0);
    let float = matches!(
        cp.sample_format,
        Some(
            symphonia::core::sample::SampleFormat::F32 | symphonia::core::sample::SampleFormat::F64
        )
    ) || codec.contains("f32")
        || codec.contains("f64");
    let mut planes: Vec<Vec<f32>> = vec![];
    let mut buf: Option<SampleBuffer<f32>> = None;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(Error::ResetRequired) => break,
            Err(e) => return Err(e).context("read packet"),
        };
        if packet.track_id() != track.id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(Error::DecodeError(_)) => continue,
            Err(Error::IoError(_)) => break,
            Err(e) => return Err(e).context("decode"),
        };
        let spec = *decoded.spec();
        if rate == 0 {
            rate = spec.rate;
        }
        let n = spec.channels.count().max(1);
        if channels == 0 {
            channels = n;
        }
        if planes.len() != n {
            planes.resize(n, vec![]);
        }
        let need = decoded.capacity() as u64;
        if buf.as_ref().is_none_or(|b| b.capacity() < need as usize) {
            buf = Some(SampleBuffer::<f32>::new(need, spec));
        }
        let b = buf.as_mut().unwrap();
        b.copy_planar_ref(decoded);
        let frames = b.samples().len() / n;
        for (c, plane) in planes.iter_mut().enumerate() {
            plane.extend_from_slice(&b.samples()[c * frames..(c + 1) * frames]);
        }
    }
    if planes.is_empty() || rate == 0 {
        bail!("no audio decoded from {}", path.display());
    }
    let audio = Audio::from_channels(rate, planes);
    let info = SoundInfo {
        rate,
        channels: audio.channels(),
        bits,
        float,
        codec,
        seconds: audio.seconds(),
        peak_db: crate::audio::gain_to_db(audio.peak()),
        recorded: bwf_recorded(path),
    };
    Ok((audio, info))
}

/// Bit depth to write for a rendered edit: "match the source" — 24-bit integer WAV, or
/// 32-bit float when the source is float.
pub fn export_bits(info: &SoundInfo) -> (u16, bool) {
    if info.float { (32, true) } else { (24, false) }
}

/// Write a WAV (atomically: temp file + rename). Samples are clipped to -1..1 for integer
/// formats.
pub fn save_wav(a: &Audio, path: &Path, bits: u16, float: bool) -> Result<()> {
    let spec = hound::WavSpec {
        channels: a.channels() as u16,
        sample_rate: a.rate,
        bits_per_sample: if float { 32 } else { bits },
        sample_format: if float {
            hound::SampleFormat::Float
        } else {
            hound::SampleFormat::Int
        },
    };
    let tmp = path.with_extension("tmp.wav");
    {
        let mut w = hound::WavWriter::create(&tmp, spec)
            .with_context(|| format!("create {}", tmp.display()))?;
        let n = a.frames();
        if float {
            for i in 0..n {
                for c in &a.ch {
                    w.write_sample(c[i])?;
                }
            }
        } else {
            let max = ((1i64 << (bits - 1)) - 1) as f32;
            for i in 0..n {
                for c in &a.ch {
                    let v = (c[i].clamp(-1.0, 1.0) * max).round() as i32;
                    w.write_sample(v)?;
                }
            }
        }
        w.finalize()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

// ------------------------------------------------------------------ overviews

/// Number of columns in a library waveform overview.
pub const OVERVIEW_COLS: usize = 480;

/// Waveform overview for library tiles: per column min / max of the mono mix, plus the file
/// facts. Small enough to cache as JSON.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Overview {
    pub info: SoundInfo,
    pub min: Vec<f32>,
    pub max: Vec<f32>,
    /// RMS per column (drawn as the darker core of the waveform).
    pub rms: Vec<f32>,
}

impl Overview {
    pub fn of(a: &Audio, info: SoundInfo, cols: usize) -> Self {
        let m = a.mix_mono();
        let (min, max, rms) = column_stats(&m, cols);
        Self {
            info,
            min,
            max,
            rms,
        }
    }
}

/// min / max / rms over `cols` equal slices of `s`.
pub fn column_stats(s: &[f32], cols: usize) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
    let cols = cols.max(1);
    let n = s.len();
    let mut mn = vec![0.0; cols];
    let mut mx = vec![0.0; cols];
    let mut rm = vec![0.0; cols];
    if n == 0 {
        return (mn, mx, rm);
    }
    for k in 0..cols {
        let a = k * n / cols;
        let b = ((k + 1) * n / cols).max(a + 1).min(n);
        let (mut lo, mut hi, mut sq) = (f32::MAX, f32::MIN, 0.0f64);
        for &v in &s[a..b] {
            lo = lo.min(v);
            hi = hi.max(v);
            sq += (v as f64) * (v as f64);
        }
        mn[k] = lo;
        mx[k] = hi;
        rm[k] = (sq / (b - a) as f64).sqrt() as f32;
    }
    (mn, mx, rm)
}

/// Disk cache file for an overview, keyed by path + size + mtime.
pub fn overview_cache_path(path: &Path, dir: &Path) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};
    let meta = std::fs::metadata(path).ok()?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    meta.len().hash(&mut h);
    meta.modified().ok()?.hash(&mut h);
    OVERVIEW_COLS.hash(&mut h);
    2u8.hash(&mut h); // format version
    Some(dir.join(format!("{:016x}.json", h.finish())))
}

/// Overview of a file, cached in `cache_dir` when given.
pub fn load_overview(path: &Path, cache_dir: Option<&Path>) -> Result<Overview> {
    let cp = cache_dir.and_then(|d| overview_cache_path(path, d));
    if let Some(cp) = &cp
        && let Ok(s) = std::fs::read_to_string(cp)
        && let Ok(o) = serde_json::from_str::<Overview>(&s)
    {
        return Ok(o);
    }
    let (a, info) = load(path)?;
    let o = Overview::of(&a, info, OVERVIEW_COLS);
    if let Some(cp) = cp {
        if let Some(d) = cp.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(cp, serde_json::to_string(&o).unwrap_or_default());
    }
    Ok(o)
}

// ------------------------------------------------------------------ BWF

/// Recording date/time from a Broadcast WAV `bext` chunk (field recorders write it).
pub fn bwf_recorded(path: &Path) -> Option<i64> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let mut hdr = [0u8; 12];
    f.read_exact(&mut hdr).ok()?;
    if &hdr[0..4] != b"RIFF" || &hdr[8..12] != b"WAVE" {
        return None;
    }
    for _ in 0..64 {
        let mut ch = [0u8; 8];
        f.read_exact(&mut ch).ok()?;
        let size = u32::from_le_bytes([ch[4], ch[5], ch[6], ch[7]]) as u64;
        if &ch[0..4] == b"bext" {
            let mut b = vec![0u8; size.min(400) as usize];
            f.read_exact(&mut b).ok()?;
            if b.len() < 338 {
                return None;
            }
            let date = std::str::from_utf8(&b[320..330])
                .ok()?
                .replace([':', '/', '.'], "-");
            let time = std::str::from_utf8(&b[330..338])
                .ok()?
                .replace(['-', '.'], ":");
            let dt = chrono::NaiveDateTime::parse_from_str(
                &format!("{} {}", date.trim(), time.trim()),
                "%Y-%m-%d %H:%M:%S",
            )
            .ok()?;
            use chrono::TimeZone;
            return chrono::Local
                .from_local_datetime(&dt)
                .single()
                .map(|t| t.timestamp());
        }
        f.seek(SeekFrom::Current((size + (size & 1)) as i64)).ok()?;
    }
    None
}
