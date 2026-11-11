//! Core tests: decoding / encoding, the cleanup DSP doing what it says, every node running
//! on mono and stereo input, recipes, and the library on disk.

use heard::audio::{Audio, db_to_gain, gain_to_db};
use heard::graph::{self, chain_graph};
use heard::library::{ItemRef, Library, Sidecar};
use heard::ops::{Op, Trim, clean, dsp, fx};
use std::f32::consts::TAU;
use std::path::Path;

const SR: u32 = 44100;

fn sine(freq: f32, secs: f32, amp: f32) -> Vec<f32> {
    (0..(secs * SR as f32) as usize)
        .map(|i| amp * (TAU * freq * i as f32 / SR as f32).sin())
        .collect()
}

fn noise(n: usize, amp: f32, seed: u32) -> Vec<f32> {
    let mut r = heard::audio::Rng::new(seed);
    (0..n).map(|_| r.bi() * amp).collect()
}

fn rms(s: &[f32]) -> f32 {
    (s.iter().map(|v| v * v).sum::<f32>() / s.len().max(1) as f32).sqrt()
}

/// Energy of `freq` in `s` (single-bin DFT), as an amplitude.
fn tone_level(s: &[f32], freq: f32) -> f32 {
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (i, v) in s.iter().enumerate() {
        let a = (TAU * freq * i as f32 / SR as f32) as f64;
        re += *v as f64 * a.cos();
        im += *v as f64 * a.sin();
    }
    (2.0 * (re * re + im * im).sqrt() / s.len() as f64) as f32
}

fn write_wav(path: &Path, a: &Audio) {
    heard::io::save_wav(a, path, 24, false).unwrap();
}

#[test]
fn wav_roundtrip_keeps_format_and_samples() {
    let dir = tempfile::tempdir().unwrap();
    let a = Audio::from_channels(48000, vec![sine(440.0, 0.5, 0.5), sine(660.0, 0.5, 0.25)]);
    let p = dir.path().join("t.wav");
    heard::io::save_wav(&a, &p, 24, false).unwrap();
    let (b, info) = heard::io::load(&p).unwrap();
    assert_eq!(
        (info.rate, info.channels, info.bits, info.float),
        (48000, 2, 24, false)
    );
    assert_eq!(b.frames(), a.frames());
    let err = a.ch[0]
        .iter()
        .zip(&b.ch[0])
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f32::max);
    assert!(err < 1e-6, "24-bit error {err}");
    assert_eq!(info.format_label(), "48k · 24-bit · stereo");
    // Float sources render as float.
    heard::io::save_wav(&a, &p, 32, true).unwrap();
    let (_, info) = heard::io::load(&p).unwrap();
    assert_eq!(heard::io::export_bits(&info), (32, true));
}

#[test]
fn bwf_recording_date_is_read() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("zoom.wav");
    write_wav(&p, &Audio::mono(SR, sine(220.0, 0.1, 0.3)));
    // Insert a bext chunk after the RIFF header (as a field recorder would).
    let data = std::fs::read(&p).unwrap();
    let mut bext = vec![0u8; 602];
    bext[320..330].copy_from_slice(b"2026-03-14");
    bext[330..338].copy_from_slice(b"07:45:10");
    let mut out = data[..12].to_vec();
    out.extend_from_slice(b"bext");
    out.extend_from_slice(&(bext.len() as u32).to_le_bytes());
    out.extend_from_slice(&bext);
    out.extend_from_slice(&data[12..]);
    let riff = (out.len() - 8) as u32;
    out[4..8].copy_from_slice(&riff.to_le_bytes());
    std::fs::write(&p, out).unwrap();
    let t = heard::io::bwf_recorded(&p).expect("bext date");
    use chrono::TimeZone;
    let d = chrono::Local.timestamp_opt(t, 0).single().unwrap();
    assert_eq!(d.format("%Y-%m-%d %H:%M").to_string(), "2026-03-14 07:45");
    // And decoding still works with the extra chunk.
    assert!(heard::io::load(&p).is_ok());
}

#[test]
fn high_pass_removes_rumble_keeps_content() {
    let s: Vec<f32> = sine(30.0, 2.0, 0.5)
        .iter()
        .zip(sine(1000.0, 2.0, 0.3))
        .map(|(a, b)| a + b)
        .collect();
    let out = clean::high_pass(Audio::mono(SR, s), 150.0, 1);
    let x = &out.ch[0][SR as usize / 2..];
    assert!(
        tone_level(x, 30.0) < 0.5 * db_to_gain(-30.0),
        "rumble {}",
        tone_level(x, 30.0)
    );
    assert!((tone_level(x, 1000.0) - 0.3).abs() < 0.02);
}

#[test]
fn hum_removal_notches_mains_and_harmonics() {
    let s: Vec<f32> = (0..SR as usize * 2)
        .map(|i| {
            let t = i as f32 / SR as f32;
            0.3 * (TAU * 50.0 * t).sin()
                + 0.15 * (TAU * 150.0 * t).sin()
                + 0.3 * (TAU * 1234.0 * t).sin()
        })
        .collect();
    let out = clean::hum(Audio::mono(SR, s), 0, 6, 30.0);
    let x = &out.ch[0][SR as usize..];
    assert!(gain_to_db(tone_level(x, 50.0) / 0.3) < -30.0);
    assert!(gain_to_db(tone_level(x, 150.0) / 0.15) < -30.0);
    assert!((tone_level(x, 1234.0) - 0.3).abs() < 0.02, "content kept");
}

#[test]
fn noise_reduction_learns_profile_and_lowers_the_floor() {
    // 1 s of hiss, then 1 s of tone + hiss, then 1 s of hiss.
    let n = SR as usize;
    let hiss = noise(n * 3, 0.05, 7);
    let tone = sine(800.0, 1.0, 0.4);
    let s: Vec<f32> = (0..n * 3)
        .map(|i| {
            hiss[i]
                + if (n..2 * n).contains(&i) {
                    tone[i - n]
                } else {
                    0.0
                }
        })
        .collect();
    let a = Audio::mono(SR, s);
    let out = clean::denoise(&a, 24.0, 1.5, 0.5, [0.1, 0.9]);
    let gap_before = rms(&a.ch[0][2 * n + n / 4..3 * n - n / 4]);
    let gap_after = rms(&out.ch[0][2 * n + n / 4..3 * n - n / 4]);
    assert!(
        gain_to_db(gap_after / gap_before) < -15.0,
        "floor {} dB",
        gain_to_db(gap_after / gap_before)
    );
    let t_after = tone_level(&out.ch[0][n + n / 4..2 * n - n / 4], 800.0);
    assert!(
        gain_to_db(t_after / 0.4).abs() < 1.5,
        "tone {} dB",
        gain_to_db(t_after / 0.4)
    );
    // Auto profile (quietest frames) works too.
    let auto = clean::denoise(&a, 24.0, 1.5, 0.5, [0.0, 0.0]);
    assert!(rms(&auto.ch[0][2 * n + n / 4..3 * n - n / 4]) < gap_before * 0.3);
    assert_eq!(out.frames(), a.frames());
}

#[test]
fn declick_removes_spikes() {
    let mut s = sine(300.0, 1.0, 0.3);
    for k in [5000, 12345, 30000] {
        s[k] = 0.95;
        s[k + 1] = -0.9;
    }
    let out = clean::declick(Audio::mono(SR, s), 3.0, 0.5);
    assert!(out.peak() < 0.35, "peak after {}", out.peak());
}

#[test]
fn normalize_peak_and_loudness() {
    let a = Audio::mono(SR, sine(997.0, 3.0, 0.1));
    let p = clean::normalize(a.clone(), 0, -1.0);
    assert!((gain_to_db(p.peak()) + 1.0).abs() < 0.05);
    // BS.1770: a full-scale 997 Hz mono sine reads about -3 LUFS.
    let full = Audio::mono(SR, sine(997.0, 3.0, 1.0));
    assert!(
        (dsp::lufs(&full) + 3.01).abs() < 0.3,
        "lufs {}",
        dsp::lufs(&full)
    );
    let l = clean::normalize(a, 1, -18.0);
    assert!((dsp::lufs(&l) + 18.0).abs() < 0.2, "lufs {}", dsp::lufs(&l));
}

#[test]
fn trims_and_time_ops_change_length_as_expected() {
    let a = Audio::from_channels(SR, vec![sine(440.0, 2.0, 0.5), sine(440.0, 2.0, 0.5)]);
    assert_eq!(clean::trim(&a, 0.5, 1.5).frames(), SR as usize);
    assert_eq!(
        clean::trim(&a, 0.5, 0.0).frames(),
        (SR as f32 * 1.5) as usize
    );
    // Auto trim: silence around a burst.
    let mut s = vec![0.0f32; SR as usize * 2];
    s[SR as usize..SR as usize + 4410].copy_from_slice(&sine(440.0, 0.1, 0.5));
    let t = clean::auto_trim(&Audio::mono(SR, s), -40.0, 10.0);
    assert!(
        (t.seconds() - 0.12).abs() < 0.01,
        "auto trim {}",
        t.seconds()
    );
    assert!((fx::varispeed(&a, -12.0, 0.0).seconds() - 4.0).abs() < 0.01);
    assert_eq!(fx::pitch_shift(&a, 5.0, 60.0).frames(), a.frames());
    assert!((fx::stretch(&a, 4.0, 0.2, 1).seconds() - 8.0).abs() < 0.01);
    assert!((fx::freeze(&a, 0.5, 3.0, 0.1, 1).seconds() - 3.0).abs() < 0.01);
    let r = fx::reverse(a.clone());
    assert_eq!(r.ch[0][0], a.ch[0][a.frames() - 1]);
    let l = clean::make_loop(&a, 250.0);
    assert!((l.seconds() - 1.75).abs() < 0.01);
}

#[test]
fn pitch_shift_moves_the_pitch() {
    let a = Audio::mono(SR, sine(440.0, 2.0, 0.5));
    let up = fx::pitch_shift(&a, 12.0, 60.0);
    let x = &up.ch[0][SR as usize / 2..];
    assert!(
        tone_level(x, 880.0) > 2.0 * tone_level(x, 440.0),
        "880 {} vs 440 {}",
        tone_level(x, 880.0),
        tone_level(x, 440.0)
    );
}

#[test]
fn onsets_find_each_hit() {
    let mut s = vec![0.0f32; SR as usize * 2];
    for k in 0..4 {
        let at = (k as f32 * 0.5 * SR as f32) as usize + 1000;
        for i in 0..2000 {
            s[at + i] = 0.6 * (-(i as f32) / 300.0).exp() * if i % 2 == 0 { 1.0 } else { -1.0 };
        }
    }
    let m = clean::onsets(&Audio::mono(SR, s), -40.0, 0.15);
    assert_eq!(m.len(), 3, "markers {m:?}");
    assert!((m[0] - (0.5 + 1000.0 / SR as f64 - 0.01)).abs() < 0.01);
}

/// Every node in the catalog, on mono and stereo, at 44.1 and 96 kHz: no panics, finite
/// output, something left.
#[test]
fn every_op_runs_on_mono_and_stereo() {
    for rate in [44100u32, 96000] {
        for chans in [1usize, 2] {
            let s: Vec<Vec<f32>> = (0..chans)
                .map(|c| {
                    let mut v = noise(rate as usize / 2, 0.2, c as u32);
                    for (i, x) in v.iter_mut().enumerate() {
                        *x += 0.3 * (TAU * 220.0 * i as f32 / rate as f32).sin();
                    }
                    v
                })
                .collect();
            let a = std::sync::Arc::new(Audio::from_channels(rate, s));
            for op in Op::catalog() {
                let inputs = vec![Some(a.clone()), Some(a.clone())];
                let out = op
                    .apply(inputs)
                    .unwrap_or_else(|| panic!("{} gave nothing", op.name()));
                assert!(out.frames() > 0, "{} empty", op.name());
                assert!(
                    out.ch.iter().flatten().all(|v| v.is_finite()),
                    "{} produced NaN/inf ({rate} Hz, {chans} ch)",
                    op.name()
                );
                assert!(out.peak() < 20.0, "{} blew up: {}", op.name(), out.peak());
            }
        }
    }
}

#[test]
fn randomize_is_deterministic_and_bounded() {
    for op in Op::catalog() {
        let (mut a, mut b) = (op.clone(), op.clone());
        a.randomize(42);
        b.randomize(42);
        assert_eq!(a, b, "{}", op.name());
    }
}

#[test]
fn identity_detection() {
    assert!(graph::is_identity(&graph::default_graph()));
    assert!(!graph::is_identity(&chain_graph(vec![Op::Gain {
        db: -3.0
    }])));
    let mut g = chain_graph(vec![Op::Reverse]);
    assert!(!graph::is_identity(&g));
    let id = graph::find(&g, |o| matches!(o, Op::Reverse)).unwrap();
    g[id].bypass = true;
    assert!(graph::is_identity(&g));
}

#[test]
fn default_recipe_is_identity() {
    let a = Audio::from_channels(SR, vec![noise(10000, 0.5, 1), noise(10000, 0.5, 2)]);
    let out = graph::render_full(&graph::default_graph(), a.clone()).unwrap();
    assert_eq!(out, a);
}

#[test]
fn presets_keep_the_sounds_trim_and_learned_profile() {
    let mut mine = graph::default_graph();
    let t = graph::trim_node(&mine).unwrap();
    mine[t].op = Op::Trim(Trim {
        start: 0.4,
        end: 1.9,
    });
    let nr = graph::find(&mine, |o| matches!(o, Op::NoiseReduction { .. })).unwrap();
    if let Op::NoiseReduction { profile, .. } = &mut mine[nr].op {
        *profile = [0.1, 0.3];
    }
    for p in heard::presets::builtin() {
        let g = heard::presets::apply(&mine, &p.graph);
        let t = graph::trim_node(&g).expect("trim kept");
        assert_eq!(
            g[t].op,
            Op::Trim(Trim {
                start: 0.4,
                end: 1.9
            }),
            "{}",
            p.name
        );
        if let Some(id) = graph::find(&g, |o| matches!(o, Op::NoiseReduction { .. }))
            && let Op::NoiseReduction { profile, .. } = &g[id].op
        {
            assert_eq!(*profile, [0.1, 0.3], "{}", p.name);
        }
        // And every preset renders.
        let a = Audio::mono(SR, sine(330.0, 2.0, 0.3));
        assert!(
            graph::render_full(&g, a).is_some_and(|o| o.frames() > 0),
            "{}",
            p.name
        );
    }
}

#[test]
fn branching_mix_patch_renders() {
    let p = heard::presets::builtin()
        .into_iter()
        .find(|p| p.name == "Shadow Double")
        .unwrap();
    let a = Audio::from_channels(SR, vec![sine(330.0, 1.0, 0.3), sine(331.0, 1.0, 0.3)]);
    let out = graph::render_full(&p.graph, a.clone()).unwrap();
    assert!(
        out.seconds() >= 2.0,
        "octave-down branch is twice as long: {}",
        out.seconds()
    );
    assert_eq!(out.channels(), 2);
}

#[test]
fn split_bands_add_back_up() {
    let a = Audio::from_channels(SR, vec![noise(20000, 0.5, 4), noise(20000, 0.5, 5)]);
    let bands = fx::split_bands(&a, 250.0, 4000.0);
    assert_eq!(bands.len(), 3);
    for c in 0..2 {
        for i in 0..a.frames() {
            let s: f32 = bands.iter().map(|b| b.ch[c][i]).sum();
            assert!((s - a.ch[c][i]).abs() < 1e-5);
        }
    }
    // Low band holds the lows.
    let tone = Audio::mono(SR, sine(80.0, 1.0, 0.5));
    let b = fx::split_bands(&tone, 250.0, 4000.0);
    assert!(b[0].rms() > 5.0 * b[2].rms());
}

#[test]
fn sidechain_gates_and_ducks() {
    let n = SR as usize;
    let a = Audio::mono(SR, sine(500.0, 2.0, 0.5));
    let mut k = vec![0.0f32; 2 * n];
    k[..n].copy_from_slice(&sine(200.0, 1.0, 0.5));
    let key = Audio::mono(SR, k);
    let gated = fx::sidechain(a.clone(), &key, 0, -30.0, 1.0, 50.0);
    assert!(rms(&gated.ch[0][n / 4..3 * n / 4]) > 0.3);
    assert!(rms(&gated.ch[0][n + n / 4..]) < 0.01);
    let ducked = fx::sidechain(a, &key, 1, -30.0, 1.0, 50.0);
    assert!(rms(&ducked.ch[0][n / 4..3 * n / 4]) < 0.01);
    assert!(rms(&ducked.ch[0][n + n / 4..]) > 0.3);
}

#[test]
fn multi_output_presets_render() {
    for name in ["Crushed Highs", "Gated Reverb"] {
        let p = heard::presets::builtin()
            .into_iter()
            .find(|p| p.name == name)
            .unwrap();
        let a = Audio::from_channels(SR, vec![noise(SR as usize, 0.3, 1); 2]);
        let out = graph::render_full(&p.graph, a.clone()).unwrap();
        assert!(out.frames() >= a.frames() && out.peak() > 0.1, "{name}");
        // Cached evaluation (preview) agrees with the full render.
        let mut ev = graph::Evaluator::new(true);
        let src = std::sync::Arc::new(a);
        let prev = ev.eval_output(&p.graph, &src, 1).unwrap();
        assert_eq!(*prev, out, "{name}");
    }
}

// ------------------------------------------------------------------ library

fn lib_with(names: &[&str]) -> (tempfile::TempDir, Library) {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::open(dir.path().join("lib")).unwrap();
    for n in names {
        write_wav(
            &lib.inbox_dir().join(n),
            &Audio::mono(SR, sine(440.0, 0.2, 0.3)),
        );
    }
    (dir, lib)
}

#[test]
fn filing_keeps_names_and_renames_together() {
    let (_d, lib) = lib_with(&["ZOOM0042.WAV", "New Recording 12.wav"]);
    lib.create_collection("Doors").unwrap();
    let items = lib.inbox_items();
    assert_eq!(items.len(), 2);
    // Filed under a planned name.
    let it = items.iter().find(|i| i.title() == "ZOOM0042.WAV").unwrap();
    let sc = Sidecar {
        name: "squeaky gate".into(),
        tags: vec!["metal".into()],
        ..Default::default()
    };
    let filed = lib.file_into(it, "Doors", &sc).unwrap();
    assert_eq!(filed.title(), "squeaky gate");
    assert!(lib.root.join("Doors/squeaky gate.wav").exists());
    assert!(lib.root.join("Doors/squeaky gate.json").exists());
    assert_eq!(lib.read_sidecar(&filed).source_name, "ZOOM0042.WAV");
    // Same name again → "-2".
    let it2 = lib.inbox_items().into_iter().next().unwrap();
    let filed2 = lib.file_into(&it2, "Doors", &sc).unwrap();
    assert_eq!(filed2.title(), "squeaky gate-2");
    // Rename moves original, edit and recipe.
    std::fs::write(lib.root.join("Doors/squeaky gate.edit.wav"), b"x").unwrap();
    let filed = lib.item(&filed.r).unwrap();
    let renamed = lib.rename_item(&filed, "gate, Hampstead").unwrap();
    assert_eq!(renamed.title(), "gate, Hampstead");
    for f in [
        "gate, Hampstead.wav",
        "gate, Hampstead.edit.wav",
        "gate, Hampstead.json",
    ] {
        assert!(lib.root.join("Doors").join(f).exists(), "{f}");
    }
    assert!(!lib.root.join("Doors/squeaky gate.wav").exists());
    // Edits aren't listed as sounds.
    assert_eq!(lib.items("Doors").len(), 2);
    assert!(
        lib.rename_item(&renamed, "squeaky gate-2").is_err(),
        "taken"
    );
    // Inbox recordings are renamed on disk (recipe follows).
    let (_d2, lib2) = lib_with(&["ZOOM0001.WAV"]);
    let it = lib2.inbox_items().remove(0);
    lib2.write_sidecar(
        &it,
        &Sidecar {
            tags: vec!["x".into()],
            ..Default::default()
        },
    )
    .unwrap();
    let r = lib2.rename_item(&it, "pigeons").unwrap();
    assert_eq!(r.title(), "pigeons.WAV");
    assert!(lib2.inbox_dir().join("pigeons.WAV").exists());
    assert_eq!(lib2.read_sidecar(&r).tags, vec!["x"]);
    // Case-only rename.
    let r2 = lib.rename_item(&renamed, "Gate, Hampstead").unwrap();
    assert_eq!(r2.title(), "Gate, Hampstead");
}

#[test]
fn import_folder_makes_collections_and_files_sounds_as_they_are() {
    let src = tempfile::tempdir().unwrap();
    let a = Audio::mono(SR, sine(220.0, 0.1, 0.3));
    std::fs::create_dir_all(src.path().join("Drone")).unwrap();
    std::fs::create_dir_all(src.path().join("Percussion/hits")).unwrap();
    std::fs::create_dir_all(src.path().join(".hidden")).unwrap();
    write_wav(&src.path().join("Drone/generator_royal_hospital.wav"), &a);
    write_wav(&src.path().join("Percussion/snare.wav"), &a);
    write_wav(&src.path().join("Percussion/hits/snare.wav"), &a);
    write_wav(&src.path().join("loose take.wav"), &a);
    write_wav(&src.path().join(".hidden/x.wav"), &a);
    std::fs::write(src.path().join("Drone/readme.txt"), "x").unwrap();
    let (_d, lib) = lib_with(&[]);
    let r = lib.import_folder(src.path()).unwrap();
    assert_eq!(r.inbox, 1);
    assert_eq!(r.filed, 3);
    assert_eq!(
        r.collections,
        vec!["Drone".to_string(), "Percussion".to_string()]
    );
    assert_eq!(lib.collections(), vec!["Drone", "Percussion"]);
    let mut names: Vec<String> = lib.items("Percussion").iter().map(|i| i.title()).collect();
    names.sort();
    assert_eq!(names, vec!["snare", "snare-2"]);
    let d = &lib.items("Drone")[0];
    assert_eq!(d.title(), "generator_royal_hospital");
    assert!(d.edit.is_none() && !d.sidecar.exists(), "filed as it is");
    // A folder that is itself a library: its Inbox feeds ours, `_presets` is skipped.
    let src2 = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(src2.path().join("Inbox")).unwrap();
    std::fs::create_dir_all(src2.path().join("_presets")).unwrap();
    write_wav(&src2.path().join("Inbox/ZOOM0001.WAV"), &a);
    let r2 = lib.import_folder(src2.path()).unwrap();
    assert_eq!((r2.inbox, r2.filed), (1, 0));
    assert!(lib.inbox_dir().join("ZOOM0001.WAV").exists());
    // Source untouched.
    assert!(
        src.path()
            .join("Drone/generator_royal_hospital.wav")
            .exists()
    );
}

#[test]
fn trash_and_restore_keep_names() {
    let (_d, lib) = lib_with(&["a.wav"]);
    lib.create_collection("Other").unwrap();
    let it = lib.inbox_items().remove(0);
    let filed = lib.file_into(&it, "Other", &Sidecar::default()).unwrap();
    let key = lib.trash(&filed).unwrap();
    assert!(lib.items("Other").is_empty());
    assert_eq!(lib.trash_items()[0].title(), "a");
    assert_eq!(lib.trash_origin(&key).as_deref(), Some("Other"));
    let back = lib.restore(&key).unwrap();
    assert_eq!(
        back.r,
        ItemRef::Filed {
            collection: "Other".into(),
            id: "a".into()
        }
    );
    assert!(lib.root.join("Other/a.json").exists());
}

#[test]
fn render_matches_the_source_format() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("in.wav");
    heard::io::save_wav(
        &Audio::from_channels(48000, vec![sine(440.0, 0.5, 0.5); 2]),
        &src,
        16,
        false,
    )
    .unwrap();
    let g = chain_graph(vec![
        Op::Trim(Trim {
            start: 0.1,
            end: 0.3,
        }),
        Op::Gain { db: -6.0 },
    ]);
    let dest = dir.path().join("in.edit.wav");
    heard::app::render_file(&src, &g, &dest).unwrap();
    let (out, info) = heard::io::load(&dest).unwrap();
    assert_eq!((info.rate, info.channels, info.bits), (48000, 2, 24));
    assert!((out.seconds() - 0.2).abs() < 0.001);
    assert!((gain_to_db(out.peak()) + 12.0).abs() < 0.2);
}

/// Decode the real collection, read-only (`cargo test -- --ignored real_sounds`).
#[test]
#[ignore]
fn real_sounds_decode() {
    let root = dirs::home_dir()
        .unwrap()
        .join("Library/Mobile Documents/com~apple~CloudDocs/Found/Sounds");
    let mut n = 0;
    for d in std::fs::read_dir(&root).unwrap().flatten() {
        for f in std::fs::read_dir(d.path()).into_iter().flatten().flatten() {
            if heard::io::is_audio(&f.path()) {
                let t = std::time::Instant::now();
                let (a, info) = heard::io::load(&f.path()).unwrap();
                let o = heard::io::Overview::of(&a, info.clone(), 480);
                println!(
                    "{:50} {} {:>7} {:.1} LUFS  {:?}",
                    f.file_name().to_string_lossy(),
                    info.format_label(),
                    heard::audio::fmt_dur(info.seconds),
                    dsp::lufs(&a),
                    t.elapsed()
                );
                assert_eq!(o.min.len(), 480);
                n += 1;
            }
        }
    }
    assert!(n > 0);
}
