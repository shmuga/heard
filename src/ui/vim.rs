//! Executes keyboard actions and `:` commands.

use crate::app::{HeardApp, Source, Tool, View};
use crate::graph::{self, Graph};
use crate::keys::{Act, Mode};
use crate::library::{Item, ItemRef};
use crate::ops::{Category, Op, Param};
use crate::presets;
use crate::settings::SortKey;
use egui_snarl::NodeId;
use std::path::PathBuf;

/// Editor slider rows: (node, param index), or (node, None) for param-less nodes (Trim).
pub fn rows(g: &Graph) -> Vec<(NodeId, Option<usize>)> {
    let mut ids = graph::chain(g);
    ids.extend(graph::off_chain(g));
    let mut out = vec![];
    for id in ids {
        let Some(n) = g.get_node(id) else { continue };
        if matches!(n.op, Op::Source | Op::Output) {
            continue;
        }
        let count = n.op.clone().params().len();
        if count == 0 {
            out.push((id, None));
        } else {
            out.extend((0..count).map(|i| (id, Some(i))));
        }
    }
    out
}

fn default_for(op: &Op) -> Op {
    Op::catalog()
        .into_iter()
        .find(|d| d.name() == op.name())
        .unwrap_or_else(|| op.clone())
}

fn adjust(op: &mut Op, idx: usize, steps: i32, coarse: bool) {
    if let Some(p) = op.params().into_iter().nth(idx) {
        match p {
            Param::F { v, min, max, .. } => {
                let st = (max - min) / 200.0 * if coarse { 10.0 } else { 1.0 };
                *v = (((*v + st * steps as f32) / st).round() * st).clamp(min, max);
            }
            Param::I { v, min, max, .. } => {
                *v = (*v + steps * if coarse { 10 } else { 1 }).clamp(min, max)
            }
            Param::B { v, .. } => {
                if steps % 2 != 0 {
                    *v = !*v;
                }
            }
            Param::C { v, options, .. } => {
                *v = (*v as i32 + steps).rem_euclid(options.len() as i32) as usize
            }
        }
    }
}

fn reset_param(op: &mut Op, idx: usize) {
    let mut d = default_for(op);
    let dv = d.params().into_iter().nth(idx).map(|p| match p {
        Param::F { v, .. } => (*v, 0, false, 0),
        Param::I { v, .. } => (0.0, *v, false, 0),
        Param::B { v, .. } => (0.0, 0, *v, 0),
        Param::C { v, .. } => (0.0, 0, false, *v),
    });
    if let (Some((f, i, b, c)), Some(p)) = (dv, op.params().into_iter().nth(idx)) {
        match p {
            Param::F { v, .. } => *v = f,
            Param::I { v, .. } => *v = i,
            Param::B { v, .. } => *v = b,
            Param::C { v, .. } => *v = c,
        }
    }
}

fn set_value(op: &mut Op, idx: usize, s: &str) -> Result<(), String> {
    let s = s.trim();
    let Some(p) = op.params().into_iter().nth(idx) else {
        return Err("no slider here".into());
    };
    match p {
        Param::F { v, min, max, .. } => {
            *v = s
                .parse::<f32>()
                .map_err(|_| format!("not a number: {s}"))?
                .clamp(min, max)
        }
        Param::I { v, min, max, .. } => {
            *v = s
                .parse::<f32>()
                .map_err(|_| format!("not a number: {s}"))?
                .round()
                .clamp(min as f32, max as f32) as i32
        }
        Param::B { v, .. } => {
            *v = match s.to_lowercase().as_str() {
                "1" | "on" | "true" | "yes" | "y" => true,
                "0" | "off" | "false" | "no" | "n" => false,
                _ => return Err(format!("expected on/off: {s}")),
            }
        }
        Param::C { v, options, .. } => {
            if let Ok(n) = s.parse::<usize>() {
                *v = n.saturating_sub(1).min(options.len() - 1);
            } else {
                let i = find_ci(options.iter().copied(), s)
                    .and_then(|m| options.iter().position(|o| *o == m))
                    .ok_or(format!("no option {s}"))?;
                *v = i;
            }
        }
    }
    Ok(())
}

/// Case-insensitive best match: exact, prefix, then substring.
pub fn find_ci<'a>(cands: impl Iterator<Item = &'a str> + Clone, q: &str) -> Option<&'a str> {
    let q = q.trim().to_lowercase();
    if q.is_empty() {
        return None;
    }
    cands
        .clone()
        .find(|c| c.to_lowercase() == q)
        .or_else(|| cands.clone().find(|c| c.to_lowercase().starts_with(&q)))
        .or_else(|| cands.clone().find(|c| c.to_lowercase().contains(&q)))
}

/// Recipe paste: everything from `src`, except the target keeps its own cut.
pub fn with_trim_of(src: &Graph, target: &Graph) -> Graph {
    crate::presets::apply(target, src)
}

/// Parse "1.5", "1:02.5", "250ms" into seconds.
pub fn parse_time(s: &str) -> Option<f64> {
    let s = s.trim();
    if let Some(ms) = s.strip_suffix("ms") {
        return ms.trim().parse::<f64>().ok().map(|v| v / 1000.0);
    }
    let s = s.trim_end_matches('s');
    match s.split_once(':') {
        Some((m, sec)) => Some(m.parse::<f64>().ok()? * 60.0 + sec.parse::<f64>().ok()?),
        None => s.parse().ok(),
    }
}

impl HeardApp {
    pub fn cursor_row(&self) -> Option<(NodeId, Option<usize>)> {
        let ed = self.editor.as_ref()?;
        rows(&ed.doc.graph).get(ed.param_cursor).copied()
    }

    pub fn cursor_param_label(&self) -> Option<String> {
        let ed = self.editor.as_ref()?;
        let (id, p) = self.cursor_row()?;
        let mut op = ed.doc.graph.get_node(id)?.op.clone();
        let name = op.name();
        let label = p.and_then(|i| {
            op.params().into_iter().nth(i).map(|p| match p {
                Param::F { label, .. }
                | Param::I { label, .. }
                | Param::B { label, .. }
                | Param::C { label, .. } => label,
            })
        });
        Some(match label {
            Some(l) => format!("{name} › {l}"),
            None => name.to_string(),
        })
    }

    fn set_cursor_node(&mut self, id: NodeId) {
        if let Some(ed) = &mut self.editor {
            if let Some(i) = rows(&ed.doc.graph).iter().position(|(n, _)| *n == id) {
                ed.param_cursor = i;
            }
            ed.selected = Some(id);
            ed.cursor_moved = true;
        }
    }

    fn clamp_cursor(&mut self) {
        if let Some(ed) = &mut self.editor {
            let n = rows(&ed.doc.graph).len();
            ed.param_cursor = ed.param_cursor.min(n.saturating_sub(1));
            ed.selected = rows(&ed.doc.graph).get(ed.param_cursor).map(|r| r.0);
            ed.cursor_moved = true;
        }
    }

    fn end_visual(&mut self) {
        self.visual_anchor = None;
        if self.keys.mode == Mode::Visual {
            self.keys.mode = Mode::Normal;
        }
    }

    fn selected_items(&self) -> Vec<Item> {
        self.selection()
            .into_iter()
            .filter_map(|i| self.items.get(i).cloned())
            .collect()
    }

    pub fn act(&mut self, ctx: &egui::Context, a: Act) {
        use Act::*;
        let view = self.view;
        match a {
            Undo => self.undo(),
            Redo => self.redo(),
            Help => self.help = !self.help,
            ToggleSidebar => {
                self.settings.sidebar_compact = !self.settings.sidebar_compact;
                self.save_settings();
            }
            Back => {
                if view != View::Library {
                    // Unsaved edits are caught by the leave guard (save / don't save / cancel).
                    self.view = View::Library;
                    self.refresh();
                }
            }
            Escape => {
                if self.confirm.is_some() {
                    self.confirm = None;
                } else if self.help {
                    self.help = false;
                } else if let Some(ed) = self
                    .editor
                    .as_mut()
                    .filter(|e| view == View::Edit && e.tool != Tool::None)
                {
                    ed.tool = Tool::None;
                } else if view == View::Edit && self.editor_playing() {
                    self.editor_play_toggle();
                } else if let Some(ed) = self
                    .editor
                    .as_mut()
                    .filter(|e| view == View::Edit && e.selection.is_some())
                {
                    ed.selection = None;
                } else if view == View::Library && self.player.is_playing() {
                    self.player.stop();
                } else if self.search.is_some() {
                    self.search = None;
                } else if !self.marks.is_empty() {
                    self.marks.clear();
                }
            }
            Go(g) => match g {
                crate::keys::Go::Inbox => self.show_source(Source::Inbox),
                crate::keys::Go::Library => self.view = View::Library,
                crate::keys::Go::Edit => {
                    if self.editor.is_some() {
                        self.view = View::Edit;
                    }
                }
                crate::keys::Go::Settings => self.view = View::Settings,
                crate::keys::Go::Trash => self.show_source(Source::Trash),
            },
            CollectionStep(n) => {
                let mut list = vec![Source::Inbox];
                list.extend(self.collections.iter().cloned().map(Source::Collection));
                let cur = list.iter().position(|s| *s == self.source).unwrap_or(0) as i32;
                let next = (cur + n).rem_euclid(list.len() as i32) as usize;
                self.show_source(list[next].clone());
            }
            ZoomIn | ZoomOut | ZoomFit | Zoom100 => self.zoom(&a),
            RunCommand(line) => self.run_command(ctx, &line),
            Search(q) => {
                self.search = (!q.is_empty()).then(|| q.to_lowercase());
                if view == View::Library && self.search.is_some() {
                    self.jump_match(0, 1);
                }
            }
            SearchNext(n) => self.jump_match(if n > 0 { 1 } else { -1 }, n.signum()),
            Complete(line) => {
                let (line, cands) = self.complete(&line);
                self.keys.line = line;
                self.completions = cands;
            }
            // ---------------- library
            Move { dx, dy } if view == View::Library => {
                let n = self.items.len();
                if n == 0 {
                    return;
                }
                let mut c = (self.cursor as i64 + dx as i64).clamp(0, n as i64 - 1) as usize;
                for _ in 0..dy.unsigned_abs() {
                    c = self.vertical_neighbor(c, dy > 0);
                }
                let moved = c != self.cursor;
                self.cursor = c;
                self.cursor_moved = true;
                if moved && self.settings.audition {
                    self.audition_toggle(ctx, c);
                }
            }
            RowStart => self.cursor = self.row_bounds(self.cursor).0,
            RowEnd => self.cursor = self.row_bounds(self.cursor).1,
            First => match view {
                View::Library => {
                    self.cursor = 0;
                    self.cursor_moved = true;
                }
                View::Edit => {
                    if let Some(ed) = &mut self.editor {
                        ed.param_cursor = 0;
                    }
                    self.clamp_cursor();
                }
                _ => {}
            },
            Last => match view {
                View::Library => {
                    self.cursor = self.items.len().saturating_sub(1);
                    self.cursor_moved = true;
                }
                View::Edit => {
                    if let Some(ed) = &mut self.editor {
                        ed.param_cursor = usize::MAX;
                    }
                    self.clamp_cursor();
                }
                _ => {}
            },
            Page(p) => self.act(ctx, Move { dx: 0, dy: p * 2 }),
            Open => {
                if let Some(it) = self.items.get(self.cursor).cloned() {
                    self.end_visual();
                    self.open_item(ctx, it);
                }
            }
            ToggleMark => {
                if let Some(it) = self.items.get(self.cursor) {
                    let p = it.orig.clone();
                    if !self.marks.remove(&p) {
                        self.marks.insert(p);
                    }
                }
            }
            VisualStart => self.visual_anchor = Some(self.cursor),
            VisualEnd => self.visual_anchor = None,
            YankRecipe => {
                let g = match view {
                    View::Edit => self.editor.as_ref().map(|e| e.doc.graph.clone()),
                    _ => self.lib.as_ref().and_then(|l| {
                        self.items
                            .get(self.cursor)
                            .map(|it| l.read_sidecar(it).graph)
                    }),
                };
                if let Some(g) = g {
                    self.yank_recipe = Some(g);
                    self.status = "Recipe yanked — p in the library pastes it onto the selection (each keeps its trim)".into();
                }
                self.end_visual();
            }
            Yank => match view {
                View::Edit => {
                    if let Some((id, _)) = self.cursor_row()
                        && let Some(n) = self.editor.as_ref().and_then(|e| e.doc.graph.get_node(id))
                    {
                        self.status = format!("Yanked node {}", n.op.name());
                        self.yank_node = Some(n.clone());
                    }
                }
                _ => self.act(ctx, YankRecipe),
            },
            Paste { before } => match view {
                View::Library => self.paste_recipe(ctx),
                View::Edit => self.paste_node(before),
                _ => {}
            },
            Delete => match view {
                View::Edit => {
                    if let Some((id, _)) = self.cursor_row()
                        && let Some(ed) = &mut self.editor
                    {
                        self.yank_node = ed.doc.graph.get_node(id).cloned(); // dd yanks, like vim
                        graph::remove_bridging(&mut ed.doc.graph, id);
                        self.clamp_cursor();
                    }
                }
                View::Library => {
                    let items = self.selected_items();
                    if self.source == Source::Trash {
                        let keys = HeardApp::trash_keys_of(&items);
                        if !keys.is_empty() {
                            self.confirm = Some(crate::app::Confirm::Purge(keys));
                        }
                    } else {
                        self.trash_items(items);
                    }
                }
                _ => {}
            },
            Restore => {
                if self.view == View::Library && self.source == Source::Trash {
                    let keys = HeardApp::trash_keys_of(&self.selected_items());
                    self.restore_keys(&keys);
                    self.end_visual();
                } else {
                    self.status = "r restores sounds in the trash (gx)".into();
                }
            }
            ItemStep(n) => {
                let cur = self
                    .editor
                    .as_ref()
                    .and_then(|e| self.items.iter().position(|i| i.orig == e.item.orig))
                    .unwrap_or(self.cursor) as i64;
                let next = cur + n as i64;
                if next >= 0
                    && let Some(it) = self.items.get(next as usize).cloned()
                {
                    self.cursor = next as usize;
                    self.open_item(ctx, it);
                }
            }
            // ---------------- editor
            ParamStep(n) => {
                if let Some(ed) = &mut self.editor {
                    ed.param_cursor = (ed.param_cursor as i64 + n as i64).max(0) as usize;
                }
                self.clamp_cursor();
            }
            NodeStep(n) => {
                if let Some(ed) = &mut self.editor {
                    let r = rows(&ed.doc.graph);
                    let cur = r.get(ed.param_cursor).map(|x| x.0);
                    let mut nodes: Vec<NodeId> = vec![];
                    for (id, _) in &r {
                        if nodes.last() != Some(id) {
                            nodes.push(*id);
                        }
                    }
                    let ci = cur
                        .and_then(|c| nodes.iter().position(|x| *x == c))
                        .unwrap_or(0) as i64;
                    let ni = (ci + n as i64).clamp(0, nodes.len() as i64 - 1).max(0) as usize;
                    if let Some(id) = nodes.get(ni).copied() {
                        self.set_cursor_node(id);
                    }
                }
            }
            Adjust { steps, coarse } => {
                if let Some(ed) = &mut self.editor
                    && let Some((id, p)) = rows(&ed.doc.graph).get(ed.param_cursor).copied()
                {
                    match p {
                        Some(i) => adjust(&mut ed.doc.graph[id].op, i, steps, coarse),
                        None => {
                            // Trim row: h / l nudges the start (10 ms, H / L 100 ms).
                            let len = ed.source_len() as f32;
                            if let Op::Trim(t) = &mut ed.doc.graph[id].op {
                                let d = steps as f32 * if coarse { 0.1 } else { 0.01 };
                                t.start =
                                    (t.start + d).clamp(0.0, (t.end_or(len) - 0.005).max(0.0));
                            }
                        }
                    }
                    ed.last_adjust = Some(a.clone());
                }
            }
            Repeat => {
                if let Some(last) = self.editor.as_ref().and_then(|e| e.last_adjust.clone()) {
                    self.act(ctx, last);
                }
            }
            ResetParam => {
                if let Some(ed) = &mut self.editor
                    && let Some((id, Some(i))) = rows(&ed.doc.graph).get(ed.param_cursor).copied()
                {
                    reset_param(&mut ed.doc.graph[id].op, i);
                }
            }
            ResetNode => {
                if let Some(ed) = &mut self.editor
                    && let Some((id, _)) = rows(&ed.doc.graph).get(ed.param_cursor).copied()
                {
                    let d = match &ed.doc.graph[id].op {
                        Op::Trim(_) => Op::Trim(Default::default()),
                        Op::NoiseReduction { profile, .. } => {
                            let mut d = default_for(&ed.doc.graph[id].op);
                            if let Op::NoiseReduction { profile: p, .. } = &mut d {
                                *p = *profile;
                            }
                            d
                        }
                        op => default_for(op),
                    };
                    ed.doc.graph[id].op = d;
                }
            }
            SetValue(s) => {
                if let Some(ed) = &mut self.editor
                    && let Some((id, Some(i))) = rows(&ed.doc.graph).get(ed.param_cursor).copied()
                    && let Err(e) = set_value(&mut ed.doc.graph[id].op, i, &s)
                {
                    self.status = e;
                }
            }
            NodeMove(n) => {
                if let Some((id, _)) = self.cursor_row()
                    && let Some(ed) = &mut self.editor
                {
                    for _ in 0..n.unsigned_abs() {
                        graph::move_in_chain(&mut ed.doc.graph, id, n.signum());
                    }
                    self.set_cursor_node(id);
                }
            }
            Bypass => {
                if let Some((id, _)) = self.cursor_row()
                    && let Some(ed) = &mut self.editor
                {
                    let n = &mut ed.doc.graph[id];
                    n.bypass = !n.bypass;
                }
            }
            ToggleTrim => {
                if let Some(ed) = &mut self.editor {
                    ed.tool = if ed.tool == Tool::Trim {
                        Tool::None
                    } else {
                        Tool::Trim
                    };
                    ed.patch = false;
                }
            }
            ToggleCompare => {
                if let Some(ed) = &mut self.editor {
                    ed.compare = !ed.compare;
                }
            }
            TogglePatch => {
                if let Some(ed) = &mut self.editor {
                    ed.patch = !ed.patch;
                }
            }
            ToggleSpectrogram => {
                self.settings.spectrogram = !self.settings.spectrogram;
                self.save_settings();
            }
            Play => match view {
                View::Library => self.audition_toggle(ctx, self.cursor),
                View::Edit => self.editor_play_toggle(),
                _ => {}
            },
            Seek(n) => {
                let playing = self.editor_playing();
                let pos = self.player.position();
                if let Some(ed) = &mut self.editor {
                    let len = ed.shown_len();
                    let from = if playing { pos } else { ed.playhead };
                    ed.playhead = (from + n as f64 * 0.1).clamp(0.0, len);
                    if playing {
                        self.player.seek(ed.playhead);
                    }
                }
            }
            SeekStart | SeekEnd => {
                if let Some(ed) = &mut self.editor {
                    ed.playhead = if a == SeekStart { 0.0 } else { ed.shown_len() };
                    if self.player.is_playing_key(ed.play_key()) {
                        self.player.seek(ed.playhead);
                    }
                }
            }
            AddMarker => {
                let playing = self.editor_playing();
                let pos = self.player.position();
                if let Some(ed) = &mut self.editor {
                    let t = if playing { pos } else { ed.playhead };
                    if !ed.markers.iter().any(|m| (m - t).abs() < 0.005) {
                        ed.markers.push(t);
                        ed.markers.sort_by(|a, b| a.total_cmp(b));
                    }
                    self.status = format!(
                        "Marker at {} ({} markers) — :split cuts there",
                        crate::audio::fmt_time(t),
                        ed.markers.len()
                    );
                }
            }
            Randomize => {
                if let Some((id, _)) = self.cursor_row()
                    && let Some(ed) = &mut self.editor
                    && ed.doc.graph[id].op.category() == Category::Experimental
                {
                    ed.roll = ed.roll.wrapping_add(1);
                    let r = ed.roll;
                    ed.doc.graph[id].op.randomize(r);
                }
            }
            Mutate => {
                if let Some(ed) = &mut self.editor {
                    let ids: Vec<NodeId> = ed.doc.graph.node_ids().map(|(i, _)| i).collect();
                    for id in ids {
                        if ed.doc.graph[id].op.category() == Category::Experimental {
                            ed.roll = ed.roll.wrapping_add(1);
                            let r = ed.roll;
                            ed.doc.graph[id].op.randomize(r);
                        }
                    }
                }
            }
            // ---------------- trim tool
            TrimNudge { start, ms } => self.with_trim(|t, len, _| {
                let d = ms as f32 / 1000.0;
                let end = t.end_or(len);
                if start {
                    t.start = (t.start + d).clamp(0.0, (end - 0.005).max(0.0));
                } else {
                    let e = (end + d).clamp(t.start + 0.005, len);
                    t.end = if e >= len - 1e-4 { 0.0 } else { e };
                }
            }),
            TrimAtPlayhead(start) => {
                let pos = if self.editor_playing() {
                    Some(self.player.position())
                } else {
                    None
                };
                self.with_trim(|t, len, ph| {
                    let ph = pos.unwrap_or(ph as f64) as f32;
                    if start {
                        t.start = ph.clamp(0.0, (t.end_or(len) - 0.005).max(0.0));
                    } else {
                        let e = ph.clamp(t.start + 0.005, len);
                        t.end = if e >= len - 1e-4 { 0.0 } else { e };
                    }
                })
            }
            TrimReset => self.with_trim(|t, _, _| *t = Default::default()),
            TrimDone => {
                if let Some(ed) = &mut self.editor {
                    ed.tool = Tool::None;
                }
            }
            Move { .. } => {}
        }
    }

    /// Edit the Trim node: (trim, source length, playhead) — trim mode shows the trim's input,
    /// so the playhead is in source time there.
    fn with_trim(&mut self, f: impl FnOnce(&mut crate::ops::Trim, f32, f32)) {
        let Some(ed) = &mut self.editor else { return };
        let Some(id) = graph::trim_node(&ed.doc.graph) else {
            self.status = "No Trim node (o, then Trim)".into();
            return;
        };
        let len = ed.source_len() as f32;
        let ph = ed.playhead as f32;
        if let Op::Trim(t) = &mut ed.doc.graph[id].op {
            f(t, len, ph);
        }
    }

    fn zoom(&mut self, a: &Act) {
        match self.view {
            View::Edit => {
                if let Some(ed) = &mut self.editor {
                    let len = ed.shown_len();
                    match a {
                        Act::ZoomIn => ed.view.zoom_in(len),
                        Act::ZoomOut => ed.view.zoom_out(len),
                        Act::ZoomFit => ed.view.fit(),
                        _ => match ed.selection {
                            Some((s, e)) => ed.view.show(s, e, len),
                            None => {
                                let ph = ed.playhead;
                                ed.view
                                    .show((ph - 0.25).max(0.0), (ph + 0.25).min(len), len)
                            }
                        },
                    }
                }
            }
            View::Library => {
                use crate::settings::{TILE_MAX, TILE_MIN};
                use crate::ui::library_ui::TILE_DEFAULT;
                let t = self.settings.tile_size;
                self.settings.tile_size = match a {
                    Act::ZoomIn => t * 1.25,
                    Act::ZoomOut => t * 0.8,
                    Act::ZoomFit => TILE_DEFAULT,
                    _ => 360.0,
                }
                .clamp(TILE_MIN, TILE_MAX);
                self.save_settings();
                self.cursor_moved = true;
            }
            View::Settings => {}
        }
    }

    fn jump_match(&mut self, start_offset: i64, dir: i32) {
        let n = self.items.len() as i64;
        if n == 0 || self.search.is_none() {
            return;
        }
        let dir = if dir < 0 { -1 } else { 1 };
        for k in 0..n {
            let i = (self.cursor as i64 + start_offset * dir as i64 + k * dir as i64).rem_euclid(n)
                as usize;
            if self.matches_search(i) {
                self.cursor = i;
                self.cursor_moved = true;
                return;
            }
        }
        self.status = format!(
            "Pattern not found: {}",
            self.search.clone().unwrap_or_default()
        );
    }

    fn paste_node(&mut self, before: bool) {
        let Some(node) = self.yank_node.clone() else {
            self.status = "Nothing yanked (yy on a node first)".into();
            return;
        };
        let Some(ed) = &mut self.editor else { return };
        let g = &mut ed.doc.graph;
        let chain = graph::chain(g);
        let cur = rows(g).get(ed.param_cursor).map(|r| r.0);
        let after = match (cur, before) {
            (Some(c), false) => Some(c),
            (Some(c), true) => graph::input_of(g, c, 0),
            (None, _) => chain.len().checked_sub(2).map(|i| chain[i]),
        };
        if let Some(after) = after {
            let id = graph::insert_after(g, after, node.op.clone());
            g[id].bypass = node.bypass;
            self.set_cursor_node(id);
        }
    }

    fn add_node(&mut self, name: &str, before: bool) {
        let catalog = Op::catalog();
        let names: Vec<&str> = catalog.iter().map(|o| o.name()).collect();
        let Some(found) = find_ci(names.iter().copied(), name) else {
            self.status = format!("No node called {name}");
            return;
        };
        let op = catalog.iter().find(|o| o.name() == found).cloned().unwrap();
        let Some(ed) = &mut self.editor else { return };
        let g = &mut ed.doc.graph;
        let chain = graph::chain(g);
        let cur = rows(g).get(ed.param_cursor).map(|r| r.0);
        let after = match (cur, before) {
            (Some(c), false) => Some(c),
            (Some(c), true) => graph::input_of(g, c, 0),
            (None, _) => chain.len().checked_sub(2).map(|i| chain[i]),
        };
        if let Some(after) = after {
            let id = graph::insert_after(g, after, op);
            self.set_cursor_node(id);
        }
    }

    /// Paste the yanked recipe onto the library selection (each keeps its own trim).
    fn paste_recipe(&mut self, ctx: &egui::Context) {
        let Some(src) = self.yank_recipe.clone() else {
            self.status = "Nothing yanked (yy on a sound, or Y in the editor)".into();
            return;
        };
        self.apply_graph_to_selection(ctx, |target| with_trim_of(&src, target), "Pasted recipe");
    }

    fn apply_graph_to_selection(
        &mut self,
        ctx: &egui::Context,
        f: impl Fn(&Graph) -> Graph,
        what: &str,
    ) {
        let Some(lib) = self.lib.clone() else { return };
        let items = self.selected_items();
        let mut jobs = vec![];
        for it in &items {
            let mut sc = lib.read_sidecar(it);
            sc.graph = f(&sc.graph);
            if lib.write_sidecar(it, &sc).is_ok()
                && let Some(dest) = lib.edit_path(it)
            {
                jobs.push((it.r.clone(), it.orig.clone(), dest, sc.graph.clone()));
            }
            if let Some(ed) = &mut self.editor
                && ed.item.orig == it.orig
            {
                ed.doc.graph = sc.graph.clone();
            }
        }
        self.status = format!("{what} onto {} sound(s)", items.len());
        self.render_batch(ctx, jobs);
        self.end_visual();
        self.marks.clear();
    }

    pub fn render_batch(
        &mut self,
        ctx: &egui::Context,
        jobs: Vec<(ItemRef, PathBuf, PathBuf, Graph)>,
    ) {
        if jobs.is_empty() {
            return;
        }
        self.status = format!("Rendering {} sound(s)…", jobs.len());
        // One job, processed sequentially: renders of long takes are memory-heavy.
        self.worker.spawn(ctx, move || {
            crate::app::Msg::Batch(
                jobs.into_iter()
                    .map(|(r, orig, dest, g)| {
                        let res = crate::app::render_file(&orig, &g, &dest);
                        (r, res.map_err(|e| format!("{e:#}")))
                    })
                    .collect(),
            )
        });
    }

    /// File Inbox recordings into `collection` (rendering their edits when they have any) and
    /// move filed sounds there from other collections. Used by `m` / `:file` and by dragging
    /// onto a collection.
    pub fn file_selection(&mut self, ctx: &egui::Context, collection: &str) {
        let items = self.selected_items();
        self.file_items(ctx, items, collection);
    }

    pub fn file_items(&mut self, ctx: &egui::Context, items: Vec<Item>, collection: &str) {
        let Some(lib) = self.lib.clone() else { return };
        let Some(col) =
            find_ci(self.collections.iter().map(|s| s.as_str()), collection).map(String::from)
        else {
            self.status = format!("No collection {collection} (:new {collection} creates it)");
            return;
        };
        let mut jobs = vec![];
        let (mut moved, mut err) = (0, None);
        let mut filed = 0;
        for it in items {
            match &it.r {
                ItemRef::Inbox(_) => {
                    let sc = lib.read_sidecar(&it);
                    match lib.file_into(&it, &col, &sc) {
                        Ok(new) => {
                            filed += 1;
                            if let Some(ed) = &mut self.editor
                                && ed.item.orig == it.orig
                            {
                                ed.name_input = new.title();
                                ed.item = new.clone();
                            }
                            if !graph::is_identity(&sc.graph)
                                && let Some(dest) = lib.edit_path(&new)
                            {
                                jobs.push((
                                    new.r.clone(),
                                    new.orig.clone(),
                                    dest,
                                    sc.graph.clone(),
                                ));
                            }
                        }
                        Err(e) => err = Some(format!("{e:#}")),
                    }
                }
                ItemRef::Filed {
                    collection: from, ..
                } if *from != col => match lib.move_to(&it, &col) {
                    Ok(new) => {
                        moved += 1;
                        if let Some(ed) = &mut self.editor
                            && ed.item.orig == it.orig
                        {
                            ed.item = new;
                        }
                    }
                    Err(e) => err = Some(format!("{e:#}")),
                },
                _ => {}
            }
        }
        let n = filed;
        self.end_visual();
        self.marks.clear();
        self.overviews.clear();
        self.refresh();
        self.render_batch(ctx, jobs);
        let mut msg = match (n, moved) {
            (0, 0) => "Nothing to move".to_string(),
            (n, 0) => format!("Filed {n} recording(s) into {col}"),
            (0, m) => format!("Moved {m} sound(s) to {col}"),
            (n, m) => format!("Filed {n} recording(s) and moved {m} sound(s) into {col}"),
        };
        if let Some(e) = err {
            msg.push_str(&format!(" — {e}"));
        }
        self.status = msg;
    }

    fn tag_selection(&mut self, tags: &[String], add: bool) {
        let Some(lib) = self.lib.clone() else { return };
        let items = self.selected_items();
        for it in &items {
            let mut sc = lib.read_sidecar(it);
            for t in tags {
                if add && !sc.tags.contains(t) {
                    sc.tags.push(t.clone());
                } else if !add {
                    sc.tags.retain(|x| x != t);
                }
            }
            let _ = lib.write_sidecar(it, &sc);
        }
        self.status = format!(
            "{} {} on {} sound(s)",
            if add { "Tagged" } else { "Untagged" },
            tags.join(", "),
            items.len()
        );
        self.end_visual();
        self.refresh();
    }

    /// `:` commands.
    pub fn run_command(&mut self, ctx: &egui::Context, line: &str) {
        let line = line.trim().trim_start_matches(':');
        if let Ok(n) = line.parse::<usize>() {
            // :N jumps to the Nth sound / slider, like a line number.
            match self.view {
                View::Library => {
                    self.cursor = n.saturating_sub(1).min(self.items.len().saturating_sub(1));
                    self.cursor_moved = true;
                }
                View::Edit => {
                    if let Some(ed) = &mut self.editor {
                        ed.param_cursor = n.saturating_sub(1);
                    }
                    self.clamp_cursor();
                }
                _ => {}
            }
            return;
        }
        let (cmd, arg) = line
            .split_once(' ')
            .map(|(a, b)| (a, b.trim()))
            .unwrap_or((line, ""));
        let tags = || -> Vec<String> {
            arg.split([',', ' '])
                .map(|t| t.trim().trim_start_matches('#').to_lowercase())
                .filter(|t| !t.is_empty())
                .collect()
        };
        match cmd {
            "w" | "write" => match self.view {
                View::Edit => self.save_current(ctx),
                _ => self.status = "Nothing to write here".into(),
            },
            "q" | "quit" | "close" => self.act(ctx, Act::Back),
            "q!" | "quit!" => {
                // vim: leave without saving.
                if self.view == View::Edit {
                    self.discard_editor();
                }
                self.act(ctx, Act::Back);
            }
            "e!" | "revert" => {
                if self.view == View::Edit {
                    self.discard_editor();
                }
                self.status = "Reverted to the last saved version".into();
            }
            "saveas" | "sav" | "copy" => {
                if self.view != View::Edit {
                    self.status =
                        ":saveas works in the editor (saves a copy into a collection)".into();
                } else if arg.is_empty() {
                    self.status = "Usage: :saveas Collection".into();
                } else {
                    self.save_copy(ctx, arg);
                }
            }
            "wq" | "x" => {
                self.run_command(ctx, "w");
                self.act(ctx, Act::Back);
            }
            "tag" | "untag" => {
                let t = tags();
                if t.is_empty() {
                    self.status = format!("Usage: :{cmd} name [name…]");
                    return;
                }
                let add = cmd == "tag";
                match self.view {
                    View::Edit => {
                        if let Some(ed) = &mut self.editor {
                            for x in t {
                                if add && !ed.doc.tags.contains(&x) {
                                    ed.doc.tags.push(x);
                                } else if !add {
                                    ed.doc.tags.retain(|y| *y != x);
                                }
                            }
                        }
                    }
                    _ => self.tag_selection(&t, add),
                }
            }
            "file" | "f" | "mv" => {
                let target = if arg.is_empty() {
                    self.editor
                        .as_ref()
                        .map(|e| e.file_target.clone())
                        .unwrap_or_default()
                } else {
                    arg.to_string()
                };
                if target.is_empty() {
                    self.status = "Usage: :file Collection".into();
                    return;
                }
                if self.view == View::Edit {
                    let Some(col) = find_ci(self.collections.iter().map(|s| s.as_str()), &target)
                        .map(String::from)
                    else {
                        self.status = format!("No collection {target}");
                        return;
                    };
                    self.file_current(ctx, &col);
                    if let Some(next) = self.next_inbox_item() {
                        self.open_item(ctx, next);
                    }
                } else {
                    self.file_selection(ctx, &target);
                }
            }
            "name" | "cw" => {
                if arg.is_empty() {
                    self.status = "Usage: :name New name".into();
                } else if self.view == View::Edit {
                    self.rename_open_sound(arg);
                } else if self.view == View::Library {
                    self.rename_selection(arg);
                }
            }
            "notes" | "note" => {
                if let Some(ed) = self.editor.as_mut().filter(|_| self.view == View::Edit) {
                    ed.doc.notes = arg.to_string();
                } else {
                    self.status = ":notes works in the editor".into();
                }
            }
            "rename" | "mvcol" => {
                // :rename New — renames the collection you're looking at (or the open sound's).
                let current = match (
                    &self.source,
                    self.view,
                    self.editor.as_ref().map(|e| &e.item.r),
                ) {
                    (_, View::Edit, Some(ItemRef::Filed { collection, .. })) => {
                        Some(collection.clone())
                    }
                    (Source::Collection(c), _, _) => Some(c.clone()),
                    _ => None,
                };
                match current {
                    Some(old) if !arg.is_empty() => self.rename_collection(&old, arg),
                    Some(old) => self.keys.prompt(&format!("rename {old}")),
                    None => self.status = "Open a collection first, then :rename NewName".into(),
                }
            }
            "new" | "mkcol" => {
                if let Some(lib) = &self.lib {
                    match lib.create_collection(arg) {
                        Ok(n) => {
                            self.status = format!("Created collection {n}");
                            if let Some(ed) = &mut self.editor
                                && ed.file_target.is_empty()
                            {
                                ed.file_target = n;
                            }
                        }
                        Err(e) => self.status = format!("{e:#}"),
                    }
                    self.refresh();
                }
            }
            "add" | "addb" => {
                if self.view != View::Edit {
                    self.status = ":add works in the editor".into();
                } else {
                    self.add_node(arg, cmd == "addb");
                }
            }
            "preset" | "pr" => {
                let names: Vec<String> = self.presets.iter().map(|p| p.name.clone()).collect();
                let Some(name) = find_ci(names.iter().map(|s| s.as_str()), arg).map(String::from)
                else {
                    self.status = format!("No preset {arg}");
                    return;
                };
                let preset = self
                    .presets
                    .iter()
                    .find(|p| p.name == name)
                    .cloned()
                    .unwrap();
                if self.view == View::Edit {
                    if let Some(ed) = &mut self.editor {
                        ed.doc.graph = presets::apply(&ed.doc.graph, &preset.graph);
                    }
                    self.clamp_cursor();
                    self.status = format!("Applied preset {name}");
                    self.remember_recipe(name.clone(), preset.graph.clone());
                } else {
                    self.apply_graph_to_selection(
                        ctx,
                        |g| presets::apply(g, &preset.graph),
                        &format!("Applied {name}"),
                    );
                }
            }
            "savepreset" | "sp" => {
                if let (Some(lib), Some(ed)) = (&self.lib, &self.editor)
                    && !arg.is_empty()
                {
                    self.status = match presets::save_user(&lib.presets_dir(), arg, &ed.doc.graph) {
                        Ok(_) => format!("Saved preset {arg}"),
                        Err(e) => format!("{e:#}"),
                    };
                    self.refresh();
                } else {
                    self.status = "Usage (in the editor): :savepreset Name".into();
                }
            }
            "trim" => {
                // :trim 0.5 3.2 (seconds, or m:ss, or 250ms) · :trim sel · :trim reset
                let words: Vec<&str> = arg.split_whitespace().collect();
                let sel = self.editor.as_ref().and_then(|e| e.selection);
                match words.as_slice() {
                    [] => self.act(ctx, Act::ToggleTrim),
                    ["reset" | "off"] => self.act(ctx, Act::TrimReset),
                    ["sel" | "selection"] => match sel {
                        Some((a, b))
                            if self.editor.as_ref().is_some_and(|e| e.tool == Tool::Trim) =>
                        {
                            self.with_trim(|t, _, _| {
                                t.start = a as f32;
                                t.end = b as f32;
                            })
                        }
                        _ => self.status = "Select on the waveform in trim mode (c) first".into(),
                    },
                    [a, rest @ ..] => match (parse_time(a), rest.first().map(|b| parse_time(b))) {
                        (Some(a), None) => self.with_trim(|t, _, _| t.start = a as f32),
                        (Some(a), Some(Some(b))) if b > a => self.with_trim(|t, len, _| {
                            t.start = a as f32;
                            t.end = if b as f32 >= len { 0.0 } else { b as f32 };
                        }),
                        _ => self.status = "Usage: :trim start [end] (seconds, m:ss or ms)".into(),
                    },
                }
            }
            "split" => self.split_to_inbox(false),
            "extract" => self.split_to_inbox(true),
            "markers" | "marker" => {
                let mut w = arg.split_whitespace();
                match w.next() {
                    Some("clear") | Some("none") => {
                        if let Some(ed) = &mut self.editor {
                            ed.markers.clear();
                        }
                    }
                    Some("auto") => {
                        let thr = w
                            .next()
                            .and_then(|v| v.parse::<f32>().ok())
                            .unwrap_or(-40.0);
                        if let Some(ed) = &mut self.editor
                            && let Some(p) = &ed.preview
                        {
                            ed.markers = crate::ops::clean::onsets(&p.audio, thr, 0.15);
                            self.status = format!(
                                "{} marker(s) at hits above {thr} dB — :split cuts there (:markers auto -30 for fewer)",
                                ed.markers.len()
                            );
                        }
                    }
                    Some(t) => match parse_time(t) {
                        Some(t) => {
                            if let Some(ed) = &mut self.editor {
                                ed.markers.push(t);
                                ed.markers.sort_by(|a, b| a.total_cmp(b));
                            }
                        }
                        None => self.status = "Usage: :markers auto [dB] | clear | <time>".into(),
                    },
                    None => self.act(ctx, Act::AddMarker),
                }
            }
            "learn" => {
                let id = self.editor.as_ref().and_then(|e| {
                    graph::find(&e.doc.graph, |o| matches!(o, Op::NoiseReduction { .. }))
                });
                match id {
                    Some(id) => self.learn_noise(id),
                    None => {
                        self.status = "No Noise Reduction node (o, then Noise Reduction)".into()
                    }
                }
            }
            "play" => self.act(ctx, Act::Play),
            "importfolder" | "importdir" => {
                if let Some(d) = rfd::FileDialog::new()
                    .set_title("Import a folder — its sub-folders become collections")
                    .pick_folder()
                {
                    self.import_folder(ctx, d);
                }
            }
            "sort" => {
                for w in arg.split_whitespace() {
                    match w {
                        "date" | "d" => self.settings.sort = SortKey::Date,
                        "name" | "n" => self.settings.sort = SortKey::Name,
                        "asc" | "a" | "old" => self.settings.sort_desc = false,
                        "desc" | "new" => self.settings.sort_desc = true,
                        "!" | "rev" | "reverse" => {
                            self.settings.sort_desc = !self.settings.sort_desc
                        }
                        _ => {}
                    }
                }
                self.save_settings();
                self.refresh();
            }
            "group" => {
                use crate::settings::GroupBy;
                let g = match arg {
                    "day" | "d" | "on" => GroupBy::Day,
                    "month" | "m" => GroupBy::Month,
                    "collection" | "col" | "c" => GroupBy::Collection,
                    "none" | "off" | "no" => GroupBy::None,
                    // Bare :group toggles grouping on / off.
                    _ if self.group_by() == GroupBy::None => GroupBy::Day,
                    _ => GroupBy::None,
                };
                self.set_group_by(g);
            }
            "layout" | "view" => {
                use crate::settings::GridLayout;
                let l = match arg {
                    "grid" | "g" => Some(GridLayout::Grid),
                    "rows" | "r" | "justified" => Some(GridLayout::Rows),
                    "list" | "l" => Some(GridLayout::List),
                    _ => None,
                };
                match l {
                    Some(l) => {
                        self.settings.grid_layout = l;
                        self.save_settings();
                    }
                    None => self.status = "Usage: :layout grid|rows|list".into(),
                }
            }
            "collections" => self.show_source(Source::AllCollections),
            "last" | "applylast" => {
                if self.view == View::Edit {
                    self.apply_last();
                    self.clamp_cursor();
                } else {
                    self.status = ":last works in the editor".into();
                }
            }
            "inbox" => self.show_source(Source::Inbox),
            "settings" | "set" if arg.is_empty() => self.view = View::Settings,
            "set" => {
                for w in arg.split_whitespace() {
                    match w {
                        "vim" => self.settings.vim_keys = true,
                        "novim" => self.settings.vim_keys = false,
                        "compact" => self.settings.sidebar_compact = true,
                        "nocompact" => self.settings.sidebar_compact = false,
                        "loop" => self.settings.loop_play = true,
                        "noloop" => self.settings.loop_play = false,
                        "spectrogram" | "spec" => self.settings.spectrogram = true,
                        "nospectrogram" | "nospec" => self.settings.spectrogram = false,
                        "audition" => self.settings.audition = true,
                        "noaudition" => self.settings.audition = false,
                        _ => self.status = format!("Unknown option: {w}"),
                    }
                }
                self.save_settings();
            }
            "library" | "lib" => self.close_library(),
            "noh" | "nohlsearch" => self.search = None,
            "help" | "h" => self.help = true,
            "yank" => self.act(ctx, Act::YankRecipe),
            "paste" => self.act(ctx, Act::Paste { before: false }),
            "import" => {
                if let Some(files) = rfd::FileDialog::new()
                    .add_filter("Audio", crate::io::AUDIO_EXTS)
                    .pick_files()
                {
                    self.import(files);
                }
            }
            "trash" | "rm" | "del" => {
                if self.view == View::Library {
                    self.act(ctx, Act::Delete);
                } else if self.view == View::Edit
                    && let Some(it) = self.editor.as_ref().map(|e| e.item.clone())
                {
                    // Trash the sound being edited and move on to the next one (like filing).
                    let idx = self.items.iter().position(|i| i.orig == it.orig);
                    self.trash_items(vec![it]);
                    match idx.and_then(|i| {
                        self.items
                            .get(i.min(self.items.len().saturating_sub(1)))
                            .cloned()
                    }) {
                        Some(next) => self.open_item(ctx, next),
                        None => self.view = View::Library,
                    }
                }
            }
            "restore" => {
                if self.source == Source::Trash {
                    self.view = View::Library;
                    self.act(ctx, Act::Restore);
                } else {
                    self.restore_last();
                }
            }
            "emptytrash" => self.confirm = Some(crate::app::Confirm::EmptyTrash),
            "selectall" | "all" => {
                if self.view == View::Library {
                    self.marks = self.items.iter().map(|i| i.orig.clone()).collect();
                }
            }
            _ => self.status = format!("E492: Not an editor command: {line}"),
        }
        if self.keys.mode == Mode::Visual
            && !matches!(cmd, "sort" | "group" | "noh" | "nohlsearch" | "help" | "h")
        {
            self.end_visual();
        }
    }

    /// Tab completion: returns the new line and the candidates.
    pub fn complete(&self, line: &str) -> (String, Vec<String>) {
        let (cmd, arg) = match line.split_once(' ') {
            Some((c, a)) => (c, Some(a)),
            None => (line, None),
        };
        let cands: Vec<String> = match arg {
            None => crate::keys::COMMANDS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            Some(_) => match cmd {
                "add" | "addb" => Op::catalog().iter().map(|o| o.name().to_string()).collect(),
                "preset" | "pr" => self.presets.iter().map(|p| p.name.clone()).collect(),
                "file" | "f" | "mv" | "saveas" | "sav" | "copy" => self.collections.clone(),
                "tag" | "untag" => self.tags.clone(),
                "sort" => ["date", "name", "asc", "desc"].map(String::from).to_vec(),
                "group" => ["day", "month", "collection", "none"]
                    .map(String::from)
                    .to_vec(),
                "layout" | "view" => ["grid", "rows", "list"].map(String::from).to_vec(),
                "trim" => ["sel", "reset"].map(String::from).to_vec(),
                "markers" => ["auto", "clear"].map(String::from).to_vec(),
                "set" => [
                    "vim",
                    "novim",
                    "compact",
                    "nocompact",
                    "loop",
                    "noloop",
                    "spectrogram",
                    "nospectrogram",
                    "audition",
                    "noaudition",
                ]
                .map(String::from)
                .to_vec(),
                _ => vec![],
            },
        };
        let typed = arg.unwrap_or(cmd).to_lowercase();
        let mut m: Vec<String> = cands
            .iter()
            .filter(|c| c.to_lowercase().starts_with(&typed))
            .cloned()
            .collect();
        if m.is_empty() && !typed.is_empty() {
            m = cands
                .iter()
                .filter(|c| c.to_lowercase().contains(&typed))
                .cloned()
                .collect();
        }
        m.truncate(10);
        let completed = match m.as_slice() {
            [one] => Some(one.clone()),
            [first, rest @ ..] => {
                let mut p = first.clone();
                for r in rest {
                    while !r.to_lowercase().starts_with(&p.to_lowercase()) {
                        p.pop();
                    }
                }
                (p.len() > typed.len()).then_some(p)
            }
            [] => None,
        };
        let line = match (completed, arg) {
            (Some(c), None) => format!("{c} "),
            (Some(c), Some(_)) => format!("{cmd} {c}"),
            (None, _) => line.to_string(),
        };
        (line, m)
    }
}
