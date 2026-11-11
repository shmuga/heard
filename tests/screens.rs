//! Renders screenshots of each view (wgpu, headless) to target/screens/. Run with
//! `cargo test --test screens -- --ignored`. Uses copies of the sounds in
//! `$HEARD_SAMPLES` (a folder of sub-folders) when set, else synthetic takes.

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use heard::audio::{Audio, Rng};
use heard::library::Library;
use heard::{AppConfig, HeardApp};
use std::f32::consts::TAU;
use std::path::Path;

fn synth(path: &Path, secs: f32, kind: u32) {
    let sr = 44100u32;
    let n = (secs * sr as f32) as usize;
    let mut r = Rng::new(kind);
    let s: Vec<f32> = (0..n)
        .map(|i| {
            let t = i as f32 / sr as f32;
            let env = match kind {
                0 => 0.3 + 0.05 * (t * 0.7).sin(),
                1 => (-(t % 1.3) * 6.0).exp(),
                _ => (t * 3.0).sin().abs() * 0.5,
            };
            env * (r.bi() * 0.5 + 0.5 * (TAU * 110.0 * t).sin())
        })
        .collect();
    heard::io::save_wav(
        &Audio::from_channels(sr, vec![s.clone(), s]),
        path,
        24,
        false,
    )
    .unwrap();
}

fn shot(h: &mut Harness<'static, HeardApp>, name: &str) {
    h.run();
    let img = h.render().expect("render");
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/screens");
    std::fs::create_dir_all(&dir).unwrap();
    img.save(dir.join(format!("{name}.png"))).unwrap();
}

fn click(h: &mut Harness<'static, HeardApp>, label: &str) {
    h.get_by_label(label).scroll_to_me();
    h.run();
    h.get_by_label(label).click();
    h.run();
}

fn typ(h: &mut Harness<'static, HeardApp>, s: &str) {
    for c in s.chars() {
        h.event(egui::Event::Text(c.to_string()));
        h.run();
    }
}

#[test]
#[ignore]
fn screens() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("library");
    let lib = Library::open(&root).unwrap();
    match std::env::var_os("HEARD_SAMPLES") {
        Some(src) => {
            lib.import_folder(Path::new(&src)).unwrap();
            // A folder of loose recordings lands in the Inbox: sort it into collections.
            if lib.collections().is_empty() {
                for it in lib.inbox_items() {
                    let n = it.title().to_lowercase();
                    let col = if n.contains("generator") || n.contains("drone") {
                        "Drone"
                    } else if n.contains("snare") || n.contains("hit") {
                        "Percussion"
                    } else if n.contains("word") || n.contains("voice") {
                        "Speech"
                    } else {
                        "Other"
                    };
                    lib.create_collection(col).unwrap();
                    lib.file_into(&it, col, &Default::default()).unwrap();
                }
            }
        }
        None => {
            let src = dir.path().join("src");
            for (c, names) in [
                ("Drone", vec!["generator"]),
                ("Percussion", vec!["snare", "hatch"]),
                ("Other", vec!["door", "till", "keys"]),
            ] {
                std::fs::create_dir_all(src.join(c)).unwrap();
                for (k, n) in names.iter().enumerate() {
                    synth(
                        &src.join(c).join(format!("{n}.wav")),
                        2.0 + k as f32 * 3.0,
                        k as u32,
                    );
                }
            }
            lib.import_folder(&src).unwrap();
        }
    }
    // Something in the Inbox, with a recorder-ish name.
    let all = lib.all_items();
    let first = all
        .iter()
        .find(|i| i.title().contains("4_hits"))
        .or_else(|| all.iter().find(|i| i.collection() == Some("Percussion")))
        .unwrap();
    std::fs::copy(&first.orig, lib.inbox_dir().join("ZOOM0042.WAV")).unwrap();

    let r2 = root.clone();
    let mut h = Harness::builder()
        .with_size(egui::vec2(1480.0, 920.0))
        .with_max_steps(96)
        .build_eframe(move |_cc| {
            HeardApp::new(AppConfig {
                library: Some(r2.clone()),
                sync: true,
                ..Default::default()
            })
        });
    h.run();
    click(&mut h, "Collections");
    shot(&mut h, "01-library-grid");
    h.state_mut().settings.grid_layout = heard::settings::GridLayout::List;
    shot(&mut h, "02-library-list");
    h.state_mut().settings.grid_layout = heard::settings::GridLayout::Rows;
    shot(&mut h, "03-library-rows");
    h.state_mut().settings.grid_layout = heard::settings::GridLayout::Grid;

    // Editor on the Inbox take: Field Clean + spectrogram + markers.
    click(&mut h, "Inbox");
    click(&mut h, "ZOOM0042.WAV");
    h.key_press(egui::Key::Enter);
    h.run();
    for c in ":preset field clean".chars() {
        h.event(egui::Event::Text(c.to_string()));
    }
    h.key_press(egui::Key::Enter);
    h.run();
    h.state_mut().settings.spectrogram = true;
    for c in ":markers auto -30".chars() {
        h.event(egui::Event::Text(c.to_string()));
    }
    h.key_press(egui::Key::Enter);
    h.run();
    h.state_mut().editor.as_mut().unwrap().selection = Some((1.2, 2.3));
    h.state_mut().editor.as_mut().unwrap().playhead = 0.6;
    h.run();
    h.run();
    shot(&mut h, "04-editor");
    typ(&mut h, "c");
    h.run();
    shot(&mut h, "05-trim");
    typ(&mut h, "c");
    h.key_press(egui::Key::Tab);
    h.run();
    shot(&mut h, "06-patch");
    h.key_press(egui::Key::Tab);
    h.run();
    h.state_mut().help = true;
    shot(&mut h, "07-help");
    h.state_mut().help = false;
    h.state_mut().view = heard::app::View::Settings;
    shot(&mut h, "08-settings");
    h.state_mut().lib = None;
    shot(&mut h, "09-startup");
}
