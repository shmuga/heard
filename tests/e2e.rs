//! End-to-end tests: drive the real app UI headlessly with egui_kittest (AccessKit queries,
//! simulated clicks / keys) against a temporary library, then check the files on disk.
//! Playback is silent in tests (no audio device) but keeps its state.

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use heard::audio::Audio;
use heard::library::{ItemRef, Library};
use heard::ops::Op;
use heard::{AppConfig, HeardApp, graph};
use std::f32::consts::TAU;
use std::path::{Path, PathBuf};

const SR: u32 = 44100;

/// A recording: hiss with `hits` decaying knocks, half a second apart.
fn write_take(path: &Path, secs: f32, hits: usize) {
    let n = (secs * SR as f32) as usize;
    let mut r = heard::audio::Rng::new(3);
    let mut l = vec![0.0f32; n];
    for (i, v) in l.iter_mut().enumerate() {
        *v = r.bi() * 0.01 + 0.05 * (TAU * 50.0 * i as f32 / SR as f32).sin();
    }
    for k in 0..hits {
        let at = 0.2 + k as f32 * 0.5;
        let at = (at * SR as f32) as usize;
        for i in 0..6000.min(n.saturating_sub(at)) {
            l[at + i] +=
                0.6 * (-(i as f32) / 900.0).exp() * (TAU * 180.0 * i as f32 / SR as f32).sin();
        }
    }
    let a = Audio::from_channels(SR, vec![l.clone(), l]);
    heard::io::save_wav(&a, path, 24, false).unwrap();
}

fn library_with(names: &[&str]) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("lib");
    let lib = Library::open(&root).unwrap();
    for n in names {
        write_take(&lib.inbox_dir().join(n), 2.4, 4);
    }
    (dir, root)
}

fn harness(root: &Path) -> Harness<'static, HeardApp> {
    let root = root.to_path_buf();
    let mut h = Harness::builder()
        .with_size(egui::vec2(1500.0, 1000.0))
        .with_max_steps(64)
        .build_eframe(move |_cc| {
            HeardApp::new(AppConfig {
                library: Some(root),
                sync: true,
                ..Default::default()
            })
        });
    h.run();
    h
}

fn click(h: &mut Harness<'static, HeardApp>, label: &str) {
    h.get_by_label(label).scroll_to_me();
    h.run();
    h.get_by_label(label).click();
    h.run();
}

/// Library tiles: a click selects, Enter opens.
fn open(h: &mut Harness<'static, HeardApp>, label: &str) {
    click(h, label);
    h.key_press(egui::Key::Enter);
    h.run();
}

fn typ(h: &mut Harness<'static, HeardApp>, s: &str) {
    for c in s.chars() {
        h.event(egui::Event::Text(c.to_string()));
        h.run();
    }
}

fn command(h: &mut Harness<'static, HeardApp>, cmd: &str) {
    typ(h, &format!(":{cmd}"));
    h.key_press(egui::Key::Enter);
    h.run();
}

fn create_collection(h: &mut Harness<'static, HeardApp>, name: &str) {
    h.state_mut().new_collection = name.into();
    h.run(); // the Create button appears once there's text
    click(h, "Create");
}

fn op_of<T>(app: &HeardApp, f: impl Fn(&Op) -> Option<T>) -> Option<T> {
    let g = &app.editor.as_ref()?.doc.graph;
    g.nodes().find_map(|n| f(&n.op))
}

#[test]
fn inbox_clean_name_and_file_into_collection() {
    let (_tmp, root) = library_with(&["ZOOM0042.WAV"]);
    let original = std::fs::read(root.join("Inbox/ZOOM0042.WAV")).unwrap();
    let mut h = harness(&root);
    assert_eq!(h.state().inbox_count, 1);
    create_collection(&mut h, "Doors");
    assert!(root.join("Doors").is_dir());

    open(&mut h, "ZOOM0042.WAV");
    let ed = h.state().editor.as_ref().expect("editor open");
    assert!(ed.preview.is_some(), "waveform rendered");
    assert_eq!(ed.name_input, "ZOOM0042");

    // Add a node from the menu: + Add node › Clean up › Hum Removal.
    click(&mut h, "+ Add node");
    h.get_by_label_contains("Clean up").click();
    h.run();
    click(&mut h, "Hum Removal");
    assert!(op_of(h.state(), |o| matches!(o, Op::Hum { .. }).then_some(())).is_some());
    // Turn the noise reduction up from the keyboard: it's the 3rd node card.
    let g = &h.state().editor.as_ref().unwrap().doc.graph;
    let rows = heard::ui::vim::rows(g);
    let nr = rows
        .iter()
        .position(|(id, p)| matches!(g[*id].op, Op::NoiseReduction { .. }) && *p == Some(0))
        .unwrap();
    h.state_mut().editor.as_mut().unwrap().param_cursor = nr;
    typ(&mut h, "5L");
    let amount = op_of(h.state(), |o| match o {
        Op::NoiseReduction { amount, .. } => Some(*amount),
        _ => None,
    })
    .unwrap();
    assert!(amount > 9.0, "noise reduction raised: {amount}");

    // A descriptive name instead of ZOOM0042, a tag, then file.
    h.state_mut().editor.as_mut().unwrap().name_input = "gate latch, Hampstead".into();
    h.state_mut()
        .editor
        .as_mut()
        .unwrap()
        .doc
        .tags
        .push("metal".into());
    h.run();
    click(&mut h, "File into Doors");
    for f in [
        "gate latch, Hampstead.wav",
        "gate latch, Hampstead.edit.wav",
        "gate latch, Hampstead.json",
    ] {
        assert!(root.join("Doors").join(f).exists(), "{f} written");
    }
    assert_eq!(
        std::fs::read(root.join("Doors/gate latch, Hampstead.wav")).unwrap(),
        original,
        "original untouched"
    );
    assert!(!root.join("Inbox/ZOOM0042.WAV").exists());
    let lib = Library::open(&root).unwrap();
    let it = lib.items("Doors").remove(0);
    let sc = lib.read_sidecar(&it);
    assert_eq!(sc.tags, vec!["metal"]);
    assert_eq!(sc.source_name, "ZOOM0042.WAV");
    let (edit, info) = heard::io::load(&root.join("Doors/gate latch, Hampstead.edit.wav")).unwrap();
    assert_eq!((info.rate, info.channels, info.bits), (SR, 2, 24));
    assert!(edit.frames() > 0);
}

#[test]
fn trim_from_the_keyboard_and_play() {
    let (_tmp, root) = library_with(&["take.wav"]);
    let mut h = harness(&root);
    open(&mut h, "take.wav");
    typ(&mut h, "c"); // trim tool
    assert_eq!(
        h.state().editor.as_ref().unwrap().tool,
        heard::app::Tool::Trim
    );
    typ(&mut h, "2L"); // start +200 ms
    typ(&mut h, "3{"); // end −300 ms
    typ(&mut h, "c");
    let (s, e) = op_of(h.state(), |o| match o {
        Op::Trim(t) => Some((t.start, t.end)),
        _ => None,
    })
    .unwrap();
    assert!((s - 0.2).abs() < 1e-4, "start {s}");
    assert!((e - 2.1).abs() < 1e-3, "end {e}");
    h.run();
    let len = h.state().editor.as_ref().unwrap().shown_len();
    assert!((len - 1.9).abs() < 0.01, "output is the cut: {len}");
    // Space plays, Esc stops.
    typ(&mut h, " ");
    assert!(h.state().editor_playing());
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(!h.state().editor_playing());
    assert!(h.state().editor_dirty(), "unsaved until :w");
}

#[test]
fn auto_markers_split_a_take_into_the_inbox() {
    let (_tmp, root) = library_with(&["four hits.wav"]);
    let mut h = harness(&root);
    open(&mut h, "four hits.wav");
    command(&mut h, "markers auto -20");
    assert_eq!(
        h.state().editor.as_ref().unwrap().markers.len(),
        3,
        "{}",
        h.state().status
    );
    command(&mut h, "split");
    let lib = Library::open(&root).unwrap();
    let mut names: Vec<String> = lib.inbox_items().iter().map(|i| i.title()).collect();
    names.sort();
    assert_eq!(
        names,
        [
            "four hits-01.wav",
            "four hits-02.wav",
            "four hits-03.wav",
            "four hits-04.wav",
            "four hits.wav"
        ]
    );
    let slice = lib
        .inbox_items()
        .into_iter()
        .find(|i| i.title() == "four hits-02.wav")
        .unwrap();
    assert!(
        lib.read_sidecar(&slice)
            .source_name
            .starts_with("four hits.wav · ")
    );
}

#[test]
fn presets_and_learned_noise_profile() {
    let (_tmp, root) = library_with(&["hum.wav"]);
    let mut h = harness(&root);
    open(&mut h, "hum.wav");
    // Select the quiet start (0.0–0.18 s) and learn the noise from it.
    h.state_mut().editor.as_mut().unwrap().selection = Some((0.0, 0.18));
    command(&mut h, "learn");
    let prof = op_of(h.state(), |o| match o {
        Op::NoiseReduction {
            profile, amount, ..
        } => Some((*profile, *amount)),
        _ => None,
    })
    .unwrap();
    assert_eq!(prof.0, [0.0, 0.18]);
    assert!(prof.1 > 0.0, "learning turns it on");
    command(&mut h, "preset Hum Kill");
    assert!(op_of(h.state(), |o| matches!(o, Op::Hum { .. }).then_some(())).is_some());
    command(&mut h, "preset paulstretch");
    let ed = h.state().editor.as_ref().unwrap();
    assert!(graph::has_experimental(&ed.doc.graph));
    assert!(ed.shown_len() > 20.0, "stretched ×12: {}", ed.shown_len());
}

#[test]
fn rename_in_library_and_unsaved_prompt() {
    let (_tmp, root) = library_with(&["New Recording 12.wav"]);
    let mut h = harness(&root);
    create_collection(&mut h, "Other");
    // File it as it is (m → :file).
    click(&mut h, "New Recording 12.wav");
    typ(&mut h, "m");
    typ(&mut h, "Other");
    h.key_press(egui::Key::Enter);
    h.run();
    assert!(root.join("Other/New Recording 12.wav").exists());
    assert!(
        !root.join("Other/New Recording 12.edit.wav").exists(),
        "nothing to render"
    );
    // Rename with cw.
    h.state_mut().source = heard::app::Source::Collection("Other".into());
    h.state_mut().refresh();
    h.run();
    click(&mut h, "New Recording 12");
    typ(&mut h, "cw");
    typ(&mut h, "shop till drawer");
    h.key_press(egui::Key::Enter);
    h.run();
    assert!(
        root.join("Other/shop till drawer.wav").exists(),
        "{}",
        h.state().status
    );
    // Edit then leave: the save prompt appears; N discards.
    open(&mut h, "shop till drawer");
    command(&mut h, "add reverse");
    assert!(h.state().editor_dirty());
    typ(&mut h, "q");
    assert!(h.state().leave.is_some(), "asks before leaving");
    h.key_press(egui::Key::N);
    h.run();
    assert_eq!(h.state().view, heard::app::View::Library);
    assert!(!root.join("Other/shop till drawer.edit.wav").exists());
}

#[test]
fn import_folder_and_audition() {
    let src = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(src.path().join("Drone")).unwrap();
    std::fs::create_dir_all(src.path().join("Speach")).unwrap();
    write_take(&src.path().join("Drone/generator.wav"), 1.0, 0);
    write_take(&src.path().join("Speach/man_single_word.wav"), 1.0, 1);
    let (_tmp, root) = library_with(&[]);
    let mut h = harness(&root);
    let ctx = h.ctx.clone();
    h.state_mut().import_folder(&ctx, src.path().to_path_buf());
    h.run();
    assert_eq!(
        h.state().collections,
        vec!["Drone", "Speach"],
        "{}",
        h.state().status
    );
    click(&mut h, "Collections");
    assert_eq!(h.state().items.len(), 2);
    // Space auditions the sound under the cursor.
    typ(&mut h, " ");
    h.run();
    assert!(h.state().player.is_playing(), "{}", h.state().status);
    typ(&mut h, " ");
    assert!(!h.state().player.is_playing());
    // Files and recipes are where they should be; the source is untouched.
    let lib = Library::open(&root).unwrap();
    let it = lib.items("Drone").remove(0);
    assert_eq!(
        it.r,
        ItemRef::Filed {
            collection: "Drone".into(),
            id: "generator".into()
        }
    );
    assert!(src.path().join("Drone/generator.wav").exists());
}

/// A stretch that changes the length while playing (loop on, a selection made before) must
/// play the new length, not the old span.
#[test]
fn stretch_plays_new_length() {
    let (_tmp, root) = library_with(&["knock.wav"]);
    let mut h = harness(&root);
    h.state_mut().settings.loop_play = true;
    open(&mut h, "knock.wav");
    let short = h.state().editor.as_ref().unwrap().shown_len();
    h.state_mut().editor.as_mut().unwrap().selection = Some((0.0, short));
    typ(&mut h, " ");
    assert!(h.state().player.is_playing());
    command(&mut h, "add paulstretch");
    for _ in 0..10 {
        h.run();
    }
    let ed = h.state().editor.as_ref().unwrap();
    let long = ed.shown_len();
    assert!(long > short * 4.0, "{short} -> {long}");
    assert_eq!(ed.selection, None, "the old selection is dropped");
    // Play again (stop, start) and run past the old length: still going, then loops.
    typ(&mut h, " ");
    if !h.state().player.is_playing() {
        typ(&mut h, " ");
    }
    h.state_mut().editor.as_mut().unwrap().playhead = 0.0;
    typ(&mut h, "  ");
    let mut max = 0.0f64;
    for _ in 0..((long + 1.0) * 10.0) as usize {
        h.state().player.tick_silent(0.1);
        max = max.max(h.state().player.position());
    }
    assert!(max > long - 0.5, "played to {max} of {long}");
}

/// Screen x of time `t` on the editor's waveform, and its vertical middle.
fn wave_point(app: &HeardApp, t: f64) -> egui::Pos2 {
    let (r, t0, t1) = app.editor.as_ref().unwrap().wave_map.unwrap();
    egui::pos2(
        r.min.x + ((t - t0) / (t1 - t0)) as f32 * r.width(),
        r.center().y,
    )
}

fn mouse_drag(h: &mut Harness<'static, HeardApp>, from: egui::Pos2, to: egui::Pos2) {
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    };
    h.event(egui::Event::PointerMoved(from));
    h.run();
    h.event(button(from, true));
    h.run();
    for k in 1..=8 {
        h.event(egui::Event::PointerMoved(
            from + (to - from) * (k as f32 / 8.0),
        ));
        h.run();
    }
    h.event(button(to, false));
    h.run();
}

fn trim_of(app: &HeardApp) -> heard::ops::Trim {
    let g = &app.editor.as_ref().unwrap().doc.graph;
    match &g[graph::trim_node(g).unwrap()].op {
        Op::Trim(t) => t.clone(),
        _ => unreachable!(),
    }
}

/// Trim mode: the edges drag with the mouse.
#[test]
fn trim_edges_drag() {
    let (_tmp, root) = library_with(&["take.wav"]);
    let mut h = harness(&root);
    open(&mut h, "take.wav");
    typ(&mut h, "c");
    let len = h.state().editor.as_ref().unwrap().shown_len();
    // Start edge: from 0 to about 0.5 s (the press is on the edge, the drag moves on).
    let (a, b) = (wave_point(h.state(), 0.0), wave_point(h.state(), 0.5));
    mouse_drag(&mut h, a + egui::vec2(2.0, 0.0), b);
    let t = trim_of(h.state());
    assert!((t.start - 0.5).abs() < 0.05, "start dragged: {t:?}");
    assert_eq!(h.state().editor.as_ref().unwrap().selection, None);
    // End edge: from the end to 2.0 s.
    let (a, b) = (wave_point(h.state(), len), wave_point(h.state(), 2.0));
    mouse_drag(&mut h, a - egui::vec2(2.0, 0.0), b);
    let t = trim_of(h.state());
    assert!((t.end - 2.0).abs() < 0.05, "end dragged: {t:?}");
}

/// On the result: a selection trims through the chain, or crops after a time-changing node.
#[test]
fn trim_to_selection_on_result() {
    let (_tmp, root) = library_with(&["take.wav"]);
    let mut h = harness(&root);
    open(&mut h, "take.wav");
    h.state_mut().editor.as_mut().unwrap().doc.graph = {
        let mut g = h.state().editor.as_ref().unwrap().doc.graph.clone();
        let id = graph::trim_node(&g).unwrap();
        g[id].op = Op::Trim(heard::ops::Trim {
            start: 0.4,
            end: 0.0,
        });
        g
    };
    for _ in 0..4 {
        h.run();
    }
    // The result starts at 0.4 s of the source: selecting 0.2–1.0 keeps 0.6–1.4.
    h.state_mut().editor.as_mut().unwrap().selection = Some((0.2, 1.0));
    typ(&mut h, "C");
    let t = trim_of(h.state());
    assert!(
        (t.start - 0.6).abs() < 1e-3 && (t.end - 1.4).abs() < 1e-3,
        "{t:?} · {}",
        h.state().status
    );
    for _ in 0..4 {
        h.run();
    }
    let len = h.state().editor.as_ref().unwrap().shown_len();
    assert!((len - 0.8).abs() < 0.01, "{len}");
    // After a stretch the selection can't map back: a crop goes in before Output.
    command(&mut h, "add paulstretch");
    for _ in 0..6 {
        h.run();
    }
    let long = h.state().editor.as_ref().unwrap().shown_len();
    h.state_mut().editor.as_mut().unwrap().selection = Some((1.0, 3.0));
    command(&mut h, "trim sel");
    for _ in 0..6 {
        h.run();
    }
    let ed = h.state().editor.as_ref().unwrap();
    assert!(
        (ed.shown_len() - 2.0).abs() < 0.01,
        "{long} -> {}",
        ed.shown_len()
    );
    let trims = ed
        .doc
        .graph
        .node_ids()
        .filter(|(_, n)| matches!(n.op, Op::Trim(_)))
        .count();
    assert_eq!(trims, 2);
    // The sound's own cut is untouched.
    let t = trim_of(h.state());
    assert!((t.start - 0.6).abs() < 1e-3, "{t:?}");
}

/// With snap on (the default), a mouse selection lands on zero crossings.
#[test]
fn selection_snaps_to_zero_crossings() {
    let (_tmp, root) = library_with(&["take.wav"]);
    let mut h = harness(&root);
    open(&mut h, "take.wav");
    assert!(h.state().settings.snap_zero);
    let (a, b) = (wave_point(h.state(), 0.613), wave_point(h.state(), 1.377));
    mouse_drag(&mut h, a, b);
    let ed = h.state().editor.as_ref().unwrap();
    let (s, e) = ed.selection.expect("selected");
    let audio = &ed.preview.as_ref().unwrap().audio;
    let near_zero = |t: f64| {
        let i = (t * audio.rate as f64).round() as usize;
        let v = |k: usize| audio.ch.iter().map(|c| c[k]).sum::<f32>();
        // A sign change touches this sample.
        (v(i) < 0.0) != (v(i + 1) < 0.0) || (v(i - 1) < 0.0) != (v(i) < 0.0)
    };
    assert!(near_zero(s) && near_zero(e), "{s} {e}");
    // Off: the edges follow the mouse exactly.
    command(&mut h, "set nosnap");
    let (a, b) = (wave_point(h.state(), 0.613), wave_point(h.state(), 1.377));
    mouse_drag(&mut h, a, b);
    assert!(!h.state().settings.snap_zero);
}
