//! Edit view: presentation mode (waveform + node chain with sliders) and patch mode (graph).

use super::wave::{self, Map, WaveLook, WavePeaks};
use super::{params, patch, theme};
use crate::app::{Editor, HeardApp, Msg, Preview, Shown, Tool, WaveDrag, gen_next};
use crate::audio::{fmt_time, gain_to_db};
use crate::graph;
use crate::library::ItemRef;
use crate::ops::{Category, Op};
use crate::presets::{Preset, PresetKind};
use egui::{Color32, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use egui_snarl::NodeId;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// Height of the bottom tool strip.
pub const BAR_H: f32 = 34.0;
const RULER_H: f32 = 20.0;

pub enum EdAction {
    ApplyPreset(usize),
    SavePreset(String),
    DeletePreset(String),
    FileInto(String),
    SaveCopy(String),
    Save,
    Reveal,
    Rename(String),
    Split(bool),
    AutoMarkers,
    Learn(NodeId),
}

impl HeardApp {
    /// Editor tools in the bottom bar. Left group: view, trim, before, transport, history,
    /// apply last. Right group (`right`): unsaved, save, trash, zoom.
    pub fn edit_tools(&mut self, ui: &mut Ui, ctx: &egui::Context, right: bool) {
        use theme::{Icon, tool};
        let last = self.settings.last_recipe.as_ref().map(|l| l.name.clone());
        let dirty = self.editor_dirty();
        let playing = self.editor_playing();
        let (mut looping, mut spectro) = (self.settings.loop_play, self.settings.spectrogram);
        let Some(ed) = &mut self.editor else { return };
        let len = ed.shown_len();
        let zoom = ed.view.factor(len);
        let (mut undo, mut redo, mut apply_last, mut save, mut trash, mut fit, mut play) =
            (false, false, false, false, false, false, false);
        if !right {
            if tool(
                ui,
                Icon::Sliders,
                "Presentation",
                "Presentation: waveform + sliders (Tab toggles)",
                Some(!ed.patch),
                true,
            )
            .clicked()
            {
                ed.patch = false;
            }
            if tool(
                ui,
                Icon::Patch,
                "Patch",
                "Patch: node graph (Tab toggles)",
                Some(ed.patch),
                true,
            )
            .clicked()
            {
                ed.patch = true;
            }
            ui.add_space(8.0);
            play = tool(
                ui,
                if playing { Icon::Stop } else { Icon::Play },
                if playing { "Stop" } else { "Play" },
                "Play / stop (Space) — plays the selection if there is one",
                None,
                ed.preview.is_some(),
            )
            .clicked();
            if tool(
                ui,
                Icon::Loop,
                "Loop",
                "Loop playback (:set loop)",
                Some(looping),
                true,
            )
            .clicked()
            {
                looping = !looping;
            }
            ui.add_space(8.0);
            if tool(
                ui,
                Icon::Trim,
                "Trim",
                "Trim: drag the start / end on the waveform (c)",
                Some(ed.tool == Tool::Trim),
                true,
            )
            .clicked()
            {
                ed.tool = if ed.tool == Tool::Trim {
                    Tool::None
                } else {
                    Tool::Trim
                };
                ed.patch = false;
            }
            if tool(
                ui,
                Icon::Compare,
                "Before",
                "Before / after: show and play the original (\\)",
                Some(ed.compare),
                true,
            )
            .clicked()
            {
                ed.compare = !ed.compare;
            }
            if tool(
                ui,
                Icon::Spectrum,
                "Spectrogram",
                "Spectrogram under the waveform (s)",
                Some(spectro),
                true,
            )
            .clicked()
            {
                spectro = !spectro;
            }
            ui.add_space(8.0);
            undo = tool(
                ui,
                Icon::Undo,
                "Undo",
                "Undo (u · ⌘Z)",
                None,
                ed.history.can_undo(),
            )
            .clicked();
            redo = tool(
                ui,
                Icon::Redo,
                "Redo",
                "Redo (Ctrl-r · ⇧⌘Z)",
                None,
                ed.history.can_redo(),
            )
            .clicked();
            ui.add_space(8.0);
            let tip = match &last {
                Some(n) => format!("Apply last: {n} (A · ⌘L) — keeps this sound's trim"),
                None => "Apply last — apply a preset or save a sound first (A · ⌘L)".into(),
            };
            apply_last = tool(ui, Icon::Bolt, "Apply last", &tip, None, last.is_some()).clicked();
        } else {
            // right_to_left: first added is rightmost.
            fit = tool(
                ui,
                Icon::Fit,
                "Fit",
                "Fit the whole sound (= · ⌘0); z1 zooms to the selection",
                None,
                true,
            )
            .clicked();
            ui.label(
                RichText::new(format!("×{zoom:.1}"))
                    .monospace()
                    .color(theme::TEXT_DIM),
            );
            ui.add_space(6.0);
            trash = tool(
                ui,
                Icon::Trash,
                "Move to trash",
                "Move this sound to Trash and open the next (⌘Backspace)",
                None,
                true,
            )
            .clicked();
            save = tool(
                ui,
                Icon::Save,
                "Save",
                "Save (⌘S · :w) — filed sounds also re-render",
                None,
                dirty,
            )
            .clicked();
            if dirty {
                theme::badge(ui, "unsaved", theme::ORANGE);
            }
        }
        if looping != self.settings.loop_play || spectro != self.settings.spectrogram {
            self.settings.loop_play = looping;
            self.settings.spectrogram = spectro;
            self.save_settings();
        }
        if play {
            self.editor_play_toggle();
        }
        if undo {
            self.undo();
        }
        if redo {
            self.redo();
        }
        if apply_last {
            self.apply_last();
        }
        if save {
            self.save_current(ctx);
        }
        if fit {
            self.act(ctx, crate::keys::Act::ZoomFit);
        }
        if trash {
            self.run_command(ctx, "trash");
        }
    }

    /// Render the waveform's content in the background when the recipe (or mode) changed.
    pub fn request_preview(&mut self, ctx: &egui::Context) {
        let Some(ed) = &mut self.editor else { return };
        let Some(source) = ed.source.clone() else {
            return;
        };
        let trim_mode = ed.tool == Tool::Trim;
        let trim_id = graph::trim_node(&ed.doc.graph).filter(|_| trim_mode);
        let shown = if ed.compare {
            Shown::Before
        } else if trim_id.is_some() {
            Shown::TrimInput
        } else {
            Shown::Output
        };
        // In trim mode the cut itself isn't rendered (it's an overlay): ignore it in the key.
        let mut keyed = ed.doc.graph.clone();
        if let Some(t) = trim_id
            && let Op::Trim(tr) = &mut keyed[t].op
        {
            *tr = Default::default();
        }
        let want = match shown {
            Shown::Before => "before".to_string(),
            _ => format!("{}|{:?}", graph::graph_json(&keyed), shown),
        };
        if want == ed.requested {
            return;
        }
        ed.requested = want;
        let generation = gen_next(&ed.latest_gen);
        let g = ed.doc.graph.clone();
        let ev = ed.evaluator.clone();
        let latest = ed.latest_gen.clone();
        let key = ed.source_key;
        self.worker.spawn(ctx, move || {
            let none = Msg::Preview {
                generation,
                preview: None,
                ms: 0.0,
            };
            if latest.load(Ordering::SeqCst) != generation {
                return none;
            }
            let mut ev = ev.lock().unwrap();
            if latest.load(Ordering::SeqCst) != generation {
                return none;
            }
            let t = std::time::Instant::now();
            ev.prune(&g);
            let out: Option<Arc<crate::audio::Audio>> = match (shown, trim_id) {
                (Shown::Before, _) => Some(source.clone()),
                (Shown::TrimInput, Some(tid)) => ev.eval_input(&g, &source, key, tid),
                _ => ev.eval_output(&g, &source, key),
            };
            let ms = t.elapsed().as_secs_f32() * 1000.0;
            let preview = out.map(|audio| Preview {
                peaks: Arc::new(WavePeaks::of(&audio)),
                lufs: crate::ops::dsp::lufs(&audio),
                peak_db: gain_to_db(audio.peak()),
                shown,
                audio,
            });
            Msg::Preview {
                generation,
                preview,
                ms,
            }
        });
    }

    fn request_spectrogram(&mut self, ctx: &egui::Context) {
        if !self.settings.spectrogram {
            return;
        }
        let Some(ed) = &mut self.editor else { return };
        let Some(p) = &ed.preview else { return };
        if ed.spectro_gen == ed.shown_gen || ed.spectro_gen == u64::MAX {
            return;
        }
        ed.spectro_gen = u64::MAX; // in flight
        let generation = ed.shown_gen;
        let audio = p.audio.clone();
        self.worker.spawn(ctx, move || {
            let cols = (audio.seconds() * 200.0).clamp(400.0, 4096.0) as usize;
            Msg::Spectro(generation, wave::spectrogram(&audio, cols))
        });
    }

    pub fn edit_view(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        if self.editor.is_none() {
            self.view = crate::app::View::Library;
            return;
        }
        self.request_preview(&ctx);
        self.request_spectrogram(&ctx);
        let presets = self.presets.clone();
        let collections = self.collections.clone();
        let mut actions: Vec<EdAction> = vec![];
        let playing_pos = self.editor_playing().then(|| self.player.position());
        let spectro = self.settings.spectrogram;
        let mut seek: Option<f64> = None;

        let patch_mode = self.editor.as_ref().is_some_and(|e| e.patch);
        egui::Panel::right("edit_panel")
            .resizable(true)
            .default_size(350.0)
            .frame(
                egui::Frame::new()
                    .fill(theme::PANEL)
                    .inner_margin(egui::Margin::same(8)),
            )
            .show(ui, |ui| {
                let ed = self.editor.as_mut().unwrap();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    side_panel(ui, ed, &presets, &collections, &mut actions);
                });
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::CANVAS))
            .show(ui, |ui| {
                let ed = self.editor.as_mut().unwrap();
                if patch_mode {
                    patch_center(ui, ed);
                } else {
                    wave_area(ui, ed, playing_pos, spectro, &mut seek);
                }
            });
        if let Some(t) = seek {
            self.player.seek(t);
        }

        for a in actions {
            match a {
                EdAction::ApplyPreset(i) => {
                    if let (Some(ed), Some(p)) = (&mut self.editor, self.presets.get(i).cloned()) {
                        ed.doc.graph = crate::presets::apply(&ed.doc.graph, &p.graph);
                        ed.selected = None;
                        self.status = format!("Applied preset {}", p.name);
                        self.remember_recipe(p.name.clone(), p.graph.clone());
                    }
                }
                EdAction::SavePreset(name) => {
                    if let (Some(lib), Some(ed)) = (&self.lib, &self.editor) {
                        match crate::presets::save_user(&lib.presets_dir(), &name, &ed.doc.graph) {
                            Ok(_) => self.status = format!("Saved preset {name}"),
                            Err(e) => self.status = format!("Couldn't save preset: {e:#}"),
                        }
                    }
                    self.refresh();
                }
                EdAction::DeletePreset(name) => {
                    if let Some(lib) = &self.lib {
                        let _ = crate::presets::delete_user(&lib.presets_dir(), &name);
                    }
                    self.refresh();
                }
                EdAction::FileInto(c) => {
                    self.file_current(&ctx, &c);
                    if let Some(next) = self.next_inbox_item() {
                        self.open_item(&ctx, next);
                    }
                }
                EdAction::Save => self.save_current(&ctx),
                EdAction::SaveCopy(c) => self.save_copy(&ctx, &c),
                EdAction::Reveal => {
                    if let Some(ed) = &self.editor {
                        let _ = std::process::Command::new("open")
                            .arg("-R")
                            .arg(ed.item.edit.as_ref().unwrap_or(&ed.item.orig))
                            .spawn();
                    }
                }
                EdAction::Rename(n) => self.rename_open_sound(&n),
                EdAction::Split(sel) => self.split_to_inbox(sel),
                EdAction::AutoMarkers => self.run_command(&ctx, "markers auto"),
                EdAction::Learn(id) => self.learn_noise(id),
            }
        }
    }

    /// Use the waveform selection as the noise profile of a Noise Reduction node.
    pub fn learn_noise(&mut self, id: NodeId) {
        let Some(ed) = &mut self.editor else { return };
        let Some((a, b)) = ed.selection.filter(|(a, b)| b - a > 0.02) else {
            self.status = "Select a stretch of just noise on the waveform first (drag)".into();
            return;
        };
        if ed
            .preview
            .as_ref()
            .is_some_and(|p| p.shown != Shown::Output)
        {
            self.status = "Leave trim / before mode, then select the noise".into();
            return;
        }
        // The selection is in output time; it maps onto this node's input only when nothing
        // after it changes the timing.
        let chain = graph::chain(&ed.doc.graph);
        let after_changes = chain
            .iter()
            .skip_while(|n| **n != id)
            .skip(1)
            .any(|n| !ed.doc.graph[*n].bypass && ed.doc.graph[*n].op.changes_time());
        if let Op::NoiseReduction {
            profile, amount, ..
        } = &mut ed.doc.graph[id].op
        {
            *profile = [a as f32, b as f32];
            if *amount <= 0.0 {
                *amount = 12.0;
            }
            self.status = format!("Noise profile learned from {}–{}", fmt_time(a), fmt_time(b));
            if after_changes {
                self.status.push_str(
                    " (nodes after Noise Reduction change timing — the profile may be off)",
                );
            }
        }
    }
}

// ------------------------------------------------------------------ side panel

fn side_panel(
    ui: &mut Ui,
    ed: &mut Editor,
    presets: &[Preset],
    collections: &[String],
    actions: &mut Vec<EdAction>,
) {
    let inbox = matches!(ed.item.r, ItemRef::Inbox(_));
    ui.horizontal(|ui| {
        let r = ui.add(
            egui::TextEdit::singleline(&mut ed.name_input)
                .id_salt("sound_name")
                .font(egui::FontId::proportional(16.0))
                .desired_width(ui.available_width() - 30.0)
                .hint_text("Name"),
        );
        let r = r.on_hover_text(if inbox {
            "Rename (Enter) — it's filed under this name"
        } else {
            "Rename (Enter): the original, edit and recipe are renamed together"
        });
        if r.lost_focus() {
            let current = match &ed.item.r {
                ItemRef::Filed { id, .. } => id.clone(),
                _ => ed
                    .item
                    .orig
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_string(),
            };
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) && ed.name_input.trim() != current {
                actions.push(EdAction::Rename(ed.name_input.clone()));
            } else if !inbox {
                ed.name_input = current;
            }
        }
        if ed.exporting {
            ui.spinner();
        }
    });
    let orig_name = ed
        .item
        .orig
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    match &ed.item.r {
        ItemRef::Inbox(_) | ItemRef::Trashed { .. } => ui.label(
            RichText::new(format!("Inbox · {orig_name} · not filed yet"))
                .small()
                .color(theme::TEXT_DIM),
        ),
        ItemRef::Filed { collection, .. } if ed.stale => {
            ui.horizontal(|ui| {
                ui.label(RichText::new(collection).small().color(theme::TEXT_DIM));
                theme::badge(ui, "edit.wav out of date · ⌘S renders", theme::ORANGE)
            })
            .inner
        }
        ItemRef::Filed { collection, .. } => ui.label(
            RichText::new(if ed.item.edit.is_some() {
                format!("{collection} · edit.wav up to date")
            } else {
                format!("{collection} · original (no edit)")
            })
            .small()
            .color(theme::TEXT_DIM),
        ),
    };
    if let Some(info) = &ed.info {
        let mut s = format!(
            "{} · {}",
            crate::audio::fmt_dur(info.seconds),
            info.format_label()
        );
        if let Some(t) = info.recorded {
            use chrono::TimeZone;
            if let Some(d) = chrono::Local.timestamp_opt(t, 0).single() {
                s.push_str(&format!(" · rec. {}", d.format("%-d %b %Y %H:%M")));
            }
        }
        ui.label(RichText::new(s).small().color(theme::TEXT_DIM));
    }
    if let Some(p) = ed.preview.as_ref().filter(|p| p.shown == Shown::Output) {
        ui.label(
            RichText::new(format!(
                "out: {} · peak {:.1} dBFS · {:.1} LUFS",
                crate::audio::fmt_dur(p.audio.seconds()),
                p.peak_db,
                p.lufs
            ))
            .small()
            .color(theme::TEXT_DIM),
        );
    }
    ui.add_space(6.0);

    if ed.patch {
        if let Some(p) = &ed.preview {
            let (rect, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), 90.0), Sense::hover());
            ui.painter().rect_filled(rect, 3.0, theme::CANVAS);
            let len = p.audio.seconds();
            wave::draw(
                ui.painter(),
                Map {
                    rect: rect.shrink(3.0),
                    t0: 0.0,
                    t1: len.max(1e-3),
                },
                &p.audio,
                &p.peaks,
                WaveLook::normal(),
            );
        }
        ui.add_space(6.0);
        match ed
            .selected
            .filter(|id| ed.doc.graph.get_node(*id).is_some())
        {
            Some(id) => node_card(ui, ed, id, false, actions),
            None => {
                ui.label(RichText::new("Click a node title to edit it here. Right-click the canvas to add nodes, right-click a node for bypass / randomize / remove.").small().color(theme::TEXT_DIM));
            }
        }
    } else {
        let chain = graph::chain(&ed.doc.graph);
        if chain.len() < 2 {
            ui.colored_label(
                theme::ORANGE,
                "Output isn't connected to Source — open Patch to rewire.",
            );
        }
        let cards: Vec<NodeId> = chain
            .iter()
            .copied()
            .filter(|id| !matches!(ed.doc.graph[*id].op, Op::Source | Op::Output))
            .collect();
        for id in cards {
            node_card(ui, ed, id, true, actions);
        }
        ui.horizontal(|ui| {
            ui.menu_button("+ Add node", |ui| {
                theme::popup(ui);
                let mut picked = None;
                patch::add_menu(ui, |op| picked = Some(op));
                if let Some(op) = picked {
                    let chain = graph::chain(&ed.doc.graph);
                    let after = if chain.len() >= 2 {
                        Some(chain[chain.len() - 2])
                    } else {
                        chain.first().copied()
                    };
                    if let Some(after) = after {
                        let id = graph::insert_after(&mut ed.doc.graph, after, op);
                        ed.selected = Some(id);
                    }
                }
            });
            if graph::has_experimental(&ed.doc.graph)
                && ui
                    .button("🎲 Mutate")
                    .on_hover_text("Randomize every experimental node (M)")
                    .clicked()
            {
                let ids: Vec<NodeId> = ed.doc.graph.node_ids().map(|(i, _)| i).collect();
                for id in ids {
                    if ed.doc.graph[id].op.category() == Category::Experimental {
                        ed.roll = ed.roll.wrapping_add(1);
                        let r = ed.roll;
                        ed.doc.graph[id].op.randomize(r);
                    }
                }
            }
        });
        let off = graph::off_chain(&ed.doc.graph);
        if !off.is_empty() {
            ui.add_space(6.0);
            ui.label(
                RichText::new("BRANCHES (wired in Patch)")
                    .small()
                    .color(theme::TEXT_DIM),
            );
            for id in off {
                if !matches!(ed.doc.graph[id].op, Op::Source | Op::Output) {
                    node_card(ui, ed, id, false, actions);
                }
            }
        }
    }

    ui.add_space(10.0);
    slices_section(ui, ed, actions);
    ui.add_space(10.0);
    presets_section(ui, ed, presets, actions);
    ui.add_space(10.0);
    tags_section(ui, ed);
    ui.add_space(10.0);
    file_section(ui, ed, collections, actions);
}

fn node_card(
    ui: &mut Ui,
    ed: &mut Editor,
    id: NodeId,
    in_chain: bool,
    actions: &mut Vec<EdAction>,
) {
    let cat = ed.doc.graph[id].op.category();
    let marker = match cat {
        Category::Experimental => theme::EXPERIMENTAL,
        Category::Clean => theme::BASIC,
        Category::Io => theme::IO,
    };
    let cursor = crate::ui::vim::rows(&ed.doc.graph)
        .get(ed.param_cursor)
        .copied()
        .filter(|r| r.0 == id);
    let selected = ed.selected == Some(id) || cursor.is_some();
    let tint = match cat {
        Category::Experimental => theme::TINT_EXP,
        Category::Clean => theme::TINT_BASIC,
        Category::Io => theme::TINT_IO,
    };
    let frame = egui::Frame::new()
        .fill(tint)
        .corner_radius(egui::CornerRadius::same(4))
        .stroke(if selected {
            Stroke::new(1.5, theme::ACCENT)
        } else {
            Stroke::new(1.0, tint.linear_multiply(0.8))
        });
    let scroll = std::mem::take(&mut ed.cursor_moved);
    let shown = frame.show(ui, |ui| {
        ui.set_width(ui.available_width());
        // Header = a patch object box: dark, light text, coloured inlet bar.
        let header = egui::Frame::new()
            .fill(if selected { theme::BOX_ACTIVE } else { theme::BOX })
            .corner_radius(egui::CornerRadius { nw: 4, ne: 4, sw: 0, se: 0 })
            .inner_margin(egui::Margin { left: 8, right: 4, top: 6, bottom: 3 })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                {
                    let w = &mut ui.visuals_mut().widgets;
                    w.inactive.weak_bg_fill = Color32::TRANSPARENT;
                    w.inactive.bg_fill = Color32::from_rgb(56, 58, 62);
                }
                ui.horizontal(|ui| {
                    let n = &mut ed.doc.graph[id];
                    let mut title = RichText::new(n.op.name()).strong().color(theme::BOX_TEXT);
                    if n.bypass {
                        title = title.strikethrough().color(Color32::from_rgb(160, 162, 166));
                    }
                    let about = n.op.about();
                    let r = ui
                        .add(egui::Label::new(title).sense(Sense::click()))
                        .on_hover_text(about);
                    if r.clicked() {
                        ed.selected = Some(id);
                        if let Some(i) = crate::ui::vim::rows(&ed.doc.graph).iter().position(|x| x.0 == id) {
                            ed.param_cursor = i;
                        }
                    }
                    if cat == Category::Experimental {
                        ui.label(RichText::new("experimental").small().color(theme::EXPERIMENTAL));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        if ui.small_button("🗙").on_hover_text("Remove").clicked() {
                            graph::remove_bridging(&mut ed.doc.graph, id);
                            if ed.selected == Some(id) {
                                ed.selected = None;
                            }
                            return;
                        }
                        if in_chain {
                            if ui.small_button("⏷").on_hover_text("Move later").clicked() {
                                graph::move_in_chain(&mut ed.doc.graph, id, 1);
                            }
                            if ui.small_button("⏶").on_hover_text("Move earlier").clicked() {
                                graph::move_in_chain(&mut ed.doc.graph, id, -1);
                            }
                        }
                        let n = &mut ed.doc.graph[id];
                        if cat == Category::Experimental
                            && !n.op.clone().params().is_empty()
                            && ui.small_button("🎲").on_hover_text("Randomize (R)").clicked()
                        {
                            ed.roll = ed.roll.wrapping_add(1);
                            n.op.randomize(ed.roll);
                        }
                        let mut on = !n.bypass;
                        if ui.checkbox(&mut on, "").on_hover_text("Enable / bypass (B)").changed() {
                            n.bypass = !on;
                        }
                    });
                });
            });
        theme::object_bars(ui.painter(), header.response.rect, marker);
        if ed.doc.graph.get_node(id).is_none() {
            return;
        }
        egui::Frame::new()
            .inner_margin(egui::Margin { left: 8, right: 8, top: 6, bottom: 8 })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                match ed.doc.graph[id].op.clone() {
                    Op::Trim(_) => trim_controls(ui, ed, id),
                    Op::NoiseReduction { profile, .. } => {
                        ui.horizontal_wrapped(|ui| {
                            if ui
                                .button("Learn from selection")
                                .on_hover_text("Drag over a stretch of just noise on the waveform, then click (:learn)")
                                .clicked()
                            {
                                actions.push(EdAction::Learn(id));
                            }
                            let learned = profile[1] > profile[0];
                            if learned && ui.small_button("Auto").on_hover_text("Forget the learned profile; use the quietest 10 %").clicked()
                                && let Op::NoiseReduction { profile, .. } = &mut ed.doc.graph[id].op
                            {
                                *profile = [0.0, 0.0];
                            }
                            ui.label(
                                RichText::new(if learned {
                                    format!("profile {}–{}", fmt_time(profile[0] as f64), fmt_time(profile[1] as f64))
                                } else {
                                    "profile: auto (quietest 10 %)".into()
                                })
                                .small()
                                .color(theme::TEXT_DIM),
                            );
                        });
                    }
                    Op::Mix { .. } | Op::Sidechain { .. } if in_chain => {
                        ui.label(RichText::new("Wire the second input in Patch view (Tab).").small().color(theme::TEXT_DIM));
                    }
                    Op::Split { .. } if in_chain => {
                        ui.label(RichText::new("Only the low band continues here — wire mid / high in Patch view (Tab) and Mix them back.").small().color(theme::TEXT_DIM));
                    }
                    Op::Reverse => {
                        ui.label(RichText::new("No settings — plays the sound backwards.").small().color(theme::TEXT_DIM));
                    }
                    _ => {}
                }
                let hl = cursor.and_then(|c| c.1);
                params::op_params(ui, &mut ed.doc.graph[id].op, ("card", id.0), hl, scroll);
            });
    });
    let r = shown.response.rect;
    if scroll && cursor.is_some_and(|c| c.1.is_none()) {
        ui.scroll_to_rect(r, None);
    }
    ui.add_space(6.0);
}

fn trim_controls(ui: &mut Ui, ed: &mut Editor, id: NodeId) {
    let len = ed.source_len() as f32;
    let sel = ed.selection;
    let trimming = ed.tool == Tool::Trim;
    let shown_trim_input = ed
        .preview
        .as_ref()
        .is_some_and(|p| p.shown == Shown::TrimInput);
    let Op::Trim(t) = &mut ed.doc.graph[id].op else {
        return;
    };
    let mut end = t.end_or(len);
    ui.horizontal(|ui| {
        ui.label("Start");
        ui.add(
            egui::DragValue::new(&mut t.start)
                .range(0.0..=len.max(0.0))
                .speed(0.005)
                .max_decimals(3)
                .suffix(" s"),
        );
        ui.label("End");
        if ui
            .add(
                egui::DragValue::new(&mut end)
                    .range(0.0..=len.max(0.0))
                    .speed(0.005)
                    .max_decimals(3)
                    .suffix(" s"),
            )
            .changed()
        {
            t.end = if (end - len).abs() < 1e-4 { 0.0 } else { end };
        }
        if t.end > 0.0 && t.end <= t.start {
            t.end = (t.start + 0.01).min(len);
        }
    });
    ui.horizontal_wrapped(|ui| {
        if theme::sel(ui, trimming, if trimming { "Done" } else { "Edit trim" })
            .on_hover_text("c — drag the start / end handles on the waveform")
            .clicked()
        {
            ed.tool = if trimming { Tool::None } else { Tool::Trim };
            ed.patch = false;
        }
        if let Some((a, b)) = sel
            && shown_trim_input
            && ui
                .button("Trim to selection")
                .on_hover_text("Keep only the selected part")
                .clicked()
        {
            t.start = a as f32;
            t.end = b as f32;
        }
        if ui.button("Reset").clicked() {
            *t = Default::default();
        }
        let kept = t.end_or(len) - t.start;
        ui.label(
            RichText::new(format!(
                "keeps {}",
                crate::audio::fmt_dur(kept.max(0.0) as f64)
            ))
            .small()
            .color(theme::TEXT_DIM),
        );
    });
}

fn slices_section(ui: &mut Ui, ed: &mut Editor, actions: &mut Vec<EdAction>) {
    theme::section(theme::TINT_TAN).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new("SLICE").small().strong().color(theme::TEXT));
        ui.label(
            RichText::new("Cut a long take into separate recordings: markers split what you see (the processed sound) into new Inbox files.")
                .small()
                .color(theme::TEXT_DIM),
        );
        ui.horizontal_wrapped(|ui| {
            if ui.button("Add marker").on_hover_text(", — at the playhead").clicked() {
                let t = ed.playhead;
                if !ed.markers.iter().any(|m| (m - t).abs() < 0.005) {
                    ed.markers.push(t);
                    ed.markers.sort_by(|a, b| a.total_cmp(b));
                }
            }
            if ui.button("Auto markers").on_hover_text(":markers auto — a marker before every hit / after every silence").clicked() {
                actions.push(EdAction::AutoMarkers);
            }
            if !ed.markers.is_empty() && ui.small_button("Clear").on_hover_text(":markers clear").clicked() {
                ed.markers.clear();
            }
        });
        ui.horizontal_wrapped(|ui| {
            let n = ed.markers.len();
            if ui
                .add_enabled(n > 0, egui::Button::new(format!("Split into {} (to Inbox)", n + 1)))
                .on_hover_text(":split")
                .clicked()
            {
                actions.push(EdAction::Split(false));
            }
            if ui
                .add_enabled(ed.selection.is_some(), egui::Button::new("Selection to Inbox"))
                .on_hover_text(":extract — the selected part as a new recording")
                .clicked()
            {
                actions.push(EdAction::Split(true));
            }
        });
    });
}

fn presets_section(ui: &mut Ui, ed: &mut Editor, presets: &[Preset], actions: &mut Vec<EdAction>) {
    theme::section(theme::TINT_IO).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new("PRESETS").small().strong().color(theme::TEXT));
        let pick = ed.preset_pick.min(presets.len().saturating_sub(1));
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("preset_pick")
                .width(170.0)
                .selected_text(presets.get(pick).map(|p| p.name.as_str()).unwrap_or("—"))
                .show_ui(ui, |ui| {
                    theme::popup(ui);
                    for (kind, label) in [
                        (PresetKind::Clean, "Clean up"),
                        (PresetKind::Experimental, "Experimental"),
                        (PresetKind::User, "Mine"),
                    ] {
                        if !presets.iter().any(|p| p.kind == kind) {
                            continue;
                        }
                        ui.label(RichText::new(label).small().color(theme::TEXT_DIM));
                        for (i, p) in presets.iter().enumerate().filter(|(_, p)| p.kind == kind) {
                            theme::sel_value(ui, &mut ed.preset_pick, i, &p.name);
                        }
                    }
                });
            if ui
                .button("Apply preset")
                .on_hover_text("Keeps this sound's trim")
                .clicked()
            {
                actions.push(EdAction::ApplyPreset(ed.preset_pick));
            }
        });
        if let Some(p) = presets.get(pick)
            && p.kind == PresetKind::User
            && ui.small_button(format!("Delete \"{}\"", p.name)).clicked()
        {
            actions.push(EdAction::DeletePreset(p.name.clone()));
            ed.preset_pick = 0;
        }
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut ed.preset_name)
                    .hint_text("Preset name")
                    .desired_width(150.0),
            );
            if ui
                .add_enabled(
                    !ed.preset_name.trim().is_empty(),
                    egui::Button::new("Save preset"),
                )
                .clicked()
            {
                actions.push(EdAction::SavePreset(ed.preset_name.trim().to_string()));
                ed.preset_name.clear();
            }
        });
    });
}

fn tags_section(ui: &mut Ui, ed: &mut Editor) {
    theme::section(theme::TINT_IO).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(
            RichText::new("TAGS & NOTES")
                .small()
                .strong()
                .color(theme::TEXT),
        );
        let mut remove = None;
        ui.horizontal_wrapped(|ui| {
            for (i, t) in ed.doc.tags.iter().enumerate() {
                if ui
                    .button(format!("#{t} 🗙"))
                    .on_hover_text("Remove tag")
                    .clicked()
                {
                    remove = Some(i);
                }
            }
        });
        if let Some(i) = remove {
            ed.doc.tags.remove(i);
        }
        ui.horizontal(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut ed.tag_input)
                    .hint_text("Tag (comma separated)")
                    .desired_width(170.0),
            );
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.button("Add tag").clicked() || enter {
                for t in ed
                    .tag_input
                    .split(',')
                    .map(|t| t.trim().trim_start_matches('#').to_lowercase())
                {
                    if !t.is_empty() && !ed.doc.tags.contains(&t) {
                        ed.doc.tags.push(t);
                    }
                }
                ed.tag_input.clear();
            }
        });
        ui.add(
            egui::TextEdit::multiline(&mut ed.doc.notes)
                .id_salt("notes")
                .hint_text("Notes: where, when, what it is…")
                .desired_rows(2)
                .desired_width(f32::INFINITY),
        );
    });
}

fn file_section(ui: &mut Ui, ed: &mut Editor, collections: &[String], actions: &mut Vec<EdAction>) {
    if matches!(ed.item.r, ItemRef::Trashed { .. }) {
        return;
    }
    theme::section(theme::TINT_BASIC).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let inbox = matches!(ed.item.r, ItemRef::Inbox(_));
        ui.label(
            RichText::new(if inbox { "FILE INTO COLLECTION" } else { "SAVE" })
                .small()
                .strong()
                .color(theme::TEXT),
        );
        if !inbox {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!ed.exporting, egui::Button::new("Save & render"))
                    .on_hover_text("⌘S — writes <name>.edit.wav next to the original")
                    .clicked()
                {
                    actions.push(EdAction::Save);
                }
                if ui.button("Reveal in Finder").clicked() {
                    actions.push(EdAction::Reveal);
                }
            });
        }
        if collections.is_empty() {
            ui.label(RichText::new("Create a collection in the sidebar first.").small().color(theme::TEXT));
            return;
        }
        if !collections.contains(&ed.file_target) {
            ed.file_target = collections[0].clone();
        }
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("file_target")
                .selected_text(&ed.file_target)
                .show_ui(ui, |ui| {
                    theme::popup(ui);
                    for c in collections {
                        theme::sel_value(ui, &mut ed.file_target, c.clone(), c);
                    }
                });
            if inbox && ui.button(format!("File into {}", ed.file_target)).on_hover_text("⌘↩ · :file").clicked() {
                actions.push(EdAction::FileInto(ed.file_target.clone()));
            }
            if ui
                .button(format!("Save copy to {}", ed.file_target))
                .on_hover_text("⇧⌘S · :saveas — a new sound in that collection with these edits; this one stays as it is")
                .clicked()
            {
                actions.push(EdAction::SaveCopy(ed.file_target.clone()));
            }
        });
        ui.label(
            RichText::new(if inbox {
                "File moves the recording (under the name above) and opens the next Inbox recording. Save copy keeps it in the Inbox."
            } else {
                "Save copy makes a new sound (e.g. a stretched variant next to the clean one)."
            })
            .small()
            .color(theme::TEXT_DIM),
        );
    });
}

// ------------------------------------------------------------------ centre

fn patch_center(ui: &mut Ui, ed: &mut Editor) {
    patch::show(ui, &mut ed.doc.graph, &mut ed.selected, &mut ed.roll);
}

fn wave_area(
    ui: &mut Ui,
    ed: &mut Editor,
    playing: Option<f64>,
    spectro_on: bool,
    seek: &mut Option<f64>,
) {
    let avail = ui.available_rect_before_wrap();
    let painter = ui.painter_at(avail);
    let Some(prev) = &ed.preview else {
        let msg = match &ed.load_error {
            Some(e) => format!("Couldn't load: {e}"),
            None if ed.source.is_some() => "Rendering…".into(),
            None => "Loading…".into(),
        };
        painter.text(
            avail.center(),
            egui::Align2::CENTER_CENTER,
            msg,
            egui::FontId::proportional(14.0),
            theme::TEXT_DIM,
        );
        return;
    };
    let audio = prev.audio.clone();
    let peaks = prev.peaks.clone();
    let shown = prev.shown;
    let len = audio.seconds().max(1e-4);
    let inner = avail.shrink2(vec2(14.0, 10.0));
    let ruler_r = Rect::from_min_size(inner.min, vec2(inner.width(), RULER_H));
    let body = Rect::from_min_max(pos2(inner.min.x, ruler_r.max.y + 4.0), inner.max);
    let (wave_r, spec_r) = if spectro_on {
        let h = (body.height() * 0.38).clamp(80.0, 320.0);
        (
            Rect::from_min_max(body.min, pos2(body.max.x, body.max.y - h - 6.0)),
            Some(Rect::from_min_max(
                pos2(body.min.x, body.max.y - h),
                body.max,
            )),
        )
    } else {
        (body, None)
    };

    // Follow the playhead when zoomed in.
    if let Some(t) = playing
        && !ed.view.is_fit()
    {
        let (a, b) = ed.view.range(len);
        if t > b || t < a {
            ed.view.start = t - (b - a) * 0.1;
            ed.view.clamp(len);
        }
    }
    let (t0, t1) = ed.view.range(len);
    let map = Map {
        rect: wave_r,
        t0,
        t1,
    };

    // ---- interaction
    let resp = ui.interact(
        Rect::from_min_max(ruler_r.min, body.max),
        ui.id().with("wave"),
        Sense::click_and_drag(),
    );
    let hover = resp.hover_pos();
    if resp.hovered() {
        let (zoom, scroll, cmd) =
            ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta, i.modifiers.command));
        if zoom != 1.0
            && let Some(p) = hover
        {
            ed.view.zoom_at(zoom as f64, map.t(p.x), len);
        } else if scroll != egui::Vec2::ZERO && !cmd {
            let d = if scroll.x.abs() > scroll.y.abs() {
                scroll.x
            } else {
                scroll.y
            };
            ed.view.pan(-(d as f64) * map.secs_per_px(), len);
        }
    }
    let trim_id = graph::trim_node(&ed.doc.graph);
    let trim = match (shown, trim_id.map(|id| &ed.doc.graph[id].op)) {
        (Shown::TrimInput, Some(Op::Trim(t))) => {
            Some((t.start as f64, t.end_or(len as f32) as f64))
        }
        _ => None,
    };
    let to_t = |x: f32| map.t(x).clamp(0.0, len);
    if resp.drag_started()
        && let Some(p) = resp.interact_pointer_pos()
    {
        let near = |t: f64| (map.x(t) - p.x).abs() < 7.0;
        ed.drag = if let Some((a, b)) = trim.filter(|(a, b)| near(*a) || near(*b)) {
            Some(WaveDrag::TrimEdge(
                near(a) && (!near(b) || (map.x(a) - p.x).abs() <= (map.x(b) - p.x).abs()),
            ))
        } else if shown == Shown::Output
            && p.y < ruler_r.max.y + 2.0
            && let Some(i) = ed.markers.iter().position(|m| near(*m))
        {
            Some(WaveDrag::Marker(i))
        } else {
            Some(WaveDrag::Select(to_t(p.x)))
        };
    }
    if resp.dragged()
        && let (Some(d), Some(p)) = (ed.drag, resp.interact_pointer_pos())
    {
        let t = to_t(p.x);
        match d {
            WaveDrag::Select(a) => {
                let (lo, hi) = if t < a { (t, a) } else { (a, t) };
                ed.selection = (hi - lo > map.secs_per_px() * 2.0).then_some((lo, hi));
            }
            WaveDrag::TrimEdge(start) => {
                if let Some(id) = trim_id
                    && let Op::Trim(tr) = &mut ed.doc.graph[id].op
                {
                    let end = tr.end_or(len as f32);
                    if start {
                        tr.start = (t as f32).min(end - 0.005).max(0.0);
                    } else {
                        let e = (t as f32).max(tr.start + 0.005);
                        tr.end = if e >= len as f32 - 1e-4 { 0.0 } else { e };
                    }
                }
            }
            WaveDrag::Marker(i) => {
                if let Some(m) = ed.markers.get_mut(i) {
                    *m = t;
                }
            }
        }
    }
    if resp.drag_stopped() {
        ed.drag = None;
        ed.markers.sort_by(|a, b| a.total_cmp(b));
    }
    if resp.clicked()
        && let Some(p) = resp.interact_pointer_pos()
    {
        let t = to_t(p.x);
        ed.playhead = t;
        ed.selection = None;
        if playing.is_some() {
            *seek = Some(t);
        }
    }
    if resp.double_clicked()
        && let Some(p) = resp.interact_pointer_pos()
    {
        // Select the region between the markers around the pointer (or everything).
        let t = to_t(p.x);
        let a = ed
            .markers
            .iter()
            .copied()
            .filter(|m| *m <= t)
            .fold(0.0, f64::max);
        let b = ed
            .markers
            .iter()
            .copied()
            .filter(|m| *m > t)
            .fold(len, f64::min);
        ed.selection = Some((a, b));
    }
    if resp.secondary_clicked()
        && shown == Shown::Output
        && let Some(p) = resp.interact_pointer_pos()
        && let Some(i) = ed
            .markers
            .iter()
            .position(|m| (map.x(*m) - p.x).abs() < 7.0)
    {
        ed.markers.remove(i);
    }

    // ---- drawing
    wave::ruler(&painter, ruler_r, map);
    painter.rect_filled(wave_r, 3.0, theme::WAVE_BG);
    let look = if shown == Shown::Before {
        WaveLook::dim()
    } else {
        WaveLook::normal()
    };
    if let Some((a, b)) = ed.selection {
        let r = Rect::from_x_y_ranges(
            map.x(a).max(wave_r.min.x)..=map.x(b).min(wave_r.max.x),
            wave_r.y_range(),
        );
        painter.rect_filled(r, 0.0, theme::ACCENT_SOFT);
    }
    wave::draw(&painter.with_clip_rect(wave_r), map, &audio, &peaks, look);
    if let Some((a, b)) = trim {
        // Dim what's cut, draw the handles.
        let shade = Color32::from_black_alpha(90);
        let left = Rect::from_x_y_ranges(
            wave_r.min.x..=map.x(a).clamp(wave_r.min.x, wave_r.max.x),
            wave_r.y_range(),
        );
        let right = Rect::from_x_y_ranges(
            map.x(b).clamp(wave_r.min.x, wave_r.max.x)..=wave_r.max.x,
            wave_r.y_range(),
        );
        painter.rect_filled(left, 0.0, shade);
        painter.rect_filled(right, 0.0, shade);
        for (t, is_start) in [(a, true), (b, false)] {
            let x = map.x(t);
            if x < wave_r.min.x - 1.0 || x > wave_r.max.x + 1.0 {
                continue;
            }
            painter.vline(x, wave_r.y_range(), Stroke::new(2.0, theme::ACCENT));
            let tab = Rect::from_center_size(
                pos2(x + if is_start { 6.0 } else { -6.0 }, wave_r.center().y),
                vec2(12.0, 34.0),
            );
            painter.rect_filled(tab, 3.0, theme::ACCENT);
            painter.text(
                tab.center(),
                egui::Align2::CENTER_CENTER,
                if is_start { "[" } else { "]" },
                egui::FontId::monospace(12.0),
                Color32::WHITE,
            );
        }
        painter.text(
            wave_r.center_bottom() - vec2(0.0, 6.0),
            egui::Align2::CENTER_BOTTOM,
            format!(
                "TRIM · keeps {} · drag the edges · c / Esc when done",
                crate::audio::fmt_dur(b - a)
            ),
            egui::FontId::proportional(12.5),
            theme::TEXT,
        );
    }
    if shown == Shown::Before {
        painter.text(
            wave_r.left_top() + vec2(8.0, 6.0),
            egui::Align2::LEFT_TOP,
            "BEFORE (original)",
            egui::FontId::proportional(13.0),
            theme::TEXT,
        );
    }
    if shown == Shown::Output {
        for m in &ed.markers {
            let x = map.x(*m);
            if x < wave_r.min.x || x > wave_r.max.x {
                continue;
            }
            painter.vline(
                x,
                ruler_r.min.y..=wave_r.max.y,
                Stroke::new(1.5, theme::ORANGE),
            );
            painter.add(egui::Shape::convex_polygon(
                vec![
                    pos2(x, ruler_r.min.y),
                    pos2(x + 9.0, ruler_r.min.y + 5.0),
                    pos2(x, ruler_r.min.y + 10.0),
                ],
                theme::ORANGE,
                Stroke::NONE,
            ));
        }
    }
    if let (Some(sr), Some(tex)) = (spec_r, &ed.spectro) {
        painter.rect_filled(sr, 3.0, wave::colormap(0.0));
        let uv = Rect::from_min_max(pos2((t0 / len) as f32, 0.0), pos2((t1 / len) as f32, 1.0));
        painter.image(tex.id(), sr, uv, Color32::WHITE);
        if let Some((a, b)) = ed.selection {
            let r = Rect::from_x_y_ranges(
                map.x(a).max(sr.min.x)..=map.x(b).min(sr.max.x),
                sr.y_range(),
            );
            painter.rect_stroke(
                r,
                0.0,
                Stroke::new(1.0, theme::ACCENT_SOFT),
                egui::StrokeKind::Inside,
            );
        }
        for (f, label) in [(100.0, "100"), (1000.0, "1k"), (10000.0, "10k")] {
            let nyq = audio.rate as f32 / 2.0;
            if f >= nyq {
                continue;
            }
            let u = (f / 30.0f32).ln() / (nyq / 30.0).ln();
            let y = sr.max.y - u * sr.height();
            painter.text(
                pos2(sr.min.x + 4.0, y),
                egui::Align2::LEFT_CENTER,
                label,
                egui::FontId::monospace(9.5),
                Color32::from_gray(200),
            );
        }
    } else if let Some(sr) = spec_r {
        painter.rect_filled(sr, 3.0, wave::colormap(0.0));
        painter.text(
            sr.center(),
            egui::Align2::CENTER_CENTER,
            "spectrogram…",
            egui::FontId::proportional(12.0),
            Color32::from_gray(180),
        );
    }
    // Playhead (live while playing).
    let ph = playing.unwrap_or(ed.playhead);
    let x = map.x(ph);
    if x >= wave_r.min.x && x <= wave_r.max.x {
        let bottom = spec_r.map(|s| s.max.y).unwrap_or(wave_r.max.y);
        painter.vline(x, ruler_r.min.y..=bottom, Stroke::new(1.5, theme::ACCENT));
    }
    // Readout.
    let mut info = match ed.selection {
        Some((a, b)) => format!(
            "sel {} – {} ({})",
            fmt_time(a),
            fmt_time(b),
            fmt_time(b - a)
        ),
        None => format!("@ {}", fmt_time(ph)),
    };
    if let Some(p) = hover.filter(|p| wave_r.contains(*p)) {
        info.push_str(&format!("   ·   {}", fmt_time(to_t(p.x))));
    }
    // On a pill in the waveform's top-right corner (clear of the ruler's labels).
    let g = painter.layout_no_wrap(info, egui::FontId::monospace(10.5), theme::BOX_TEXT);
    let pill = Rect::from_min_size(
        pos2(wave_r.max.x - g.size().x - 14.0, wave_r.min.y + 4.0),
        g.size() + vec2(10.0, 4.0),
    );
    painter.rect_filled(pill, 3.0, theme::BOX.gamma_multiply(0.85));
    painter.galley(pill.min + vec2(5.0, 2.0), g, theme::BOX_TEXT);
    if resp.hovered() && ed.drag.is_none() {
        let near_edge = trim.is_some_and(|(a, b)| {
            hover.is_some_and(|p| (map.x(a) - p.x).abs() < 7.0 || (map.x(b) - p.x).abs() < 7.0)
        });
        ui.ctx().set_cursor_icon(if near_edge {
            egui::CursorIcon::ResizeHorizontal
        } else {
            egui::CursorIcon::Text
        });
    }
}
