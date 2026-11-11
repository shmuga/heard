//! Application state, message handling, dialogs and the frame loop.

use crate::audio::Audio;
use crate::graph::{self, Evaluator, Graph};
use crate::io::{Overview, SoundInfo};
use crate::library::{FolderImport, Item, ItemRef, Library};
use crate::player::Player;
use crate::presets::{self, Preset};
use crate::settings::{Layout, Settings};
use crate::ui::theme;
use crate::ui::wave::{TimeView, WavePeaks};
use crate::undo::History;
use crate::worker::Worker;
use egui::{TextureHandle, Ui};
use egui_snarl::NodeId;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Default)]
pub struct AppConfig {
    /// Open this library directly (e.g. from the command line), skipping the startup picker.
    pub library: Option<PathBuf>,
    /// Where settings are read/written. `None` = in-memory only (tests).
    pub settings_file: Option<PathBuf>,
    /// Run background jobs inline and play silently (tests).
    pub sync: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Library,
    Edit,
    Settings,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Inbox,
    Collection(String),
    Tag(String),
    Trash,
    /// Every filed sound (sidebar "Collections" heading), grouped by collection by default.
    AllCollections,
}

/// Where the user wanted to go when unsaved changes stopped them (see `leave_modal`).
#[derive(Clone, Debug)]
pub enum Leave {
    View { from: View, to: View },
    OpenItem(Item),
    CloseLibrary,
    SetLibrary(PathBuf),
    Quit,
}

/// What the sidebar's inline rename field is renaming.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenameTarget {
    Collection(String),
}

/// Where dragged library sounds can be dropped (sidebar rows).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DropTarget {
    Collection(String),
    Trash,
}

/// A destructive action waiting for confirmation (permanent delete from the trash).
#[derive(Clone, Debug, PartialEq)]
pub enum Confirm {
    Purge(Vec<String>),
    EmptyTrash,
}

/// Mouse rubber-band selection in the library grid.
#[derive(Clone, Debug)]
pub struct Marquee {
    pub start: egui::Pos2,
    /// Marks before the drag (kept when ⌘ / ⇧ is held).
    pub base: BTreeSet<PathBuf>,
}

/// What the editor's waveform shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shown {
    /// The rendered result.
    Output,
    /// The original (before / after).
    Before,
    /// The Trim node's input, while trimming.
    TrimInput,
}

pub struct Preview {
    pub audio: Arc<Audio>,
    pub peaks: Arc<WavePeaks>,
    pub shown: Shown,
    pub lufs: f32,
    pub peak_db: f32,
}

pub enum Msg {
    Overview(PathBuf, Option<Arc<Overview>>),
    Loaded(ItemRef, Result<(Arc<Audio>, SoundInfo), String>),
    Preview {
        generation: u64,
        preview: Option<Preview>,
        ms: f32,
    },
    Spectro(u64, egui::ColorImage),
    Audition(PathBuf, Result<Arc<Audio>, String>),
    Exported {
        r: ItemRef,
        result: Result<PathBuf, String>,
    },
    /// Background batch render (filing / pasting recipes onto several sounds).
    Batch(Vec<(ItemRef, Result<PathBuf, String>)>),
    FolderImported(Result<FolderImport, String>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    None,
    Trim,
}

#[derive(Clone, Debug)]
pub struct Doc {
    pub graph: Graph,
    pub tags: Vec<String>,
    pub notes: String,
}

impl Doc {
    pub fn key(&self) -> String {
        format!(
            "{}|{}|{}",
            graph::graph_json(&self.graph),
            self.tags.join(","),
            self.notes
        )
    }
}

/// Mouse drag on the waveform.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WaveDrag {
    /// Drawing a selection from this time.
    Select(f64),
    /// Moving a trim edge (true = start).
    TrimEdge(bool),
    /// Moving a marker (index).
    Marker(usize),
}

pub struct Editor {
    pub item: Item,
    pub doc: Doc,
    pub history: History<Doc>,
    /// The decoded original.
    pub source: Option<Arc<Audio>>,
    pub info: Option<SoundInfo>,
    pub source_key: u64,
    pub load_error: Option<String>,
    /// What the waveform shows (rendered output, original, or the trim input).
    pub preview: Option<Preview>,
    pub spectro: Option<TextureHandle>,
    pub spectro_gen: u64,
    pub evaluator: Arc<Mutex<Evaluator>>,
    pub latest_gen: Arc<AtomicU64>,
    pub requested: String,
    pub shown_gen: u64,
    pub render_ms: f32,
    pub patch: bool,
    pub tool: Tool,
    pub compare: bool,
    pub selected: Option<NodeId>,
    pub roll: u32,
    pub tag_input: String,
    /// The sound's name (filed: its file name; Inbox: what it'll be filed as).
    pub name_input: String,
    pub file_target: String,
    pub preset_pick: usize,
    pub preset_name: String,
    /// The rendered edit.wav no longer matches the recipe.
    pub stale: bool,
    pub exporting: bool,
    /// Waveform: visible time range, selection, playhead, markers (in the shown timeline).
    pub view: TimeView,
    pub selection: Option<(f64, f64)>,
    pub playhead: f64,
    pub markers: Vec<f64>,
    pub drag: Option<WaveDrag>,
    /// Keyboard cursor over the slider rows (see `ui::vim::rows`).
    pub param_cursor: usize,
    pub cursor_moved: bool,
    pub last_adjust: Option<crate::keys::Act>,
    /// The recipe as last saved to disk; edits stay in memory until saved (⌘S / :w).
    pub saved: Doc,
}

impl Editor {
    pub fn dirty(&self) -> bool {
        self.doc.key() != self.saved.key()
    }

    /// Length of what's shown, in seconds.
    pub fn shown_len(&self) -> f64 {
        self.preview
            .as_ref()
            .map(|p| p.audio.seconds())
            .unwrap_or(0.0)
    }

    pub fn source_len(&self) -> f64 {
        self.source.as_ref().map(|s| s.seconds()).unwrap_or(0.0)
    }

    /// A key for the player: which buffer is loaded.
    pub fn play_key(&self) -> u64 {
        path_key(&self.item.orig) ^ 0x5eed
    }
}

pub struct HeardApp {
    pub lib: Option<Library>,
    pub worker: Worker<Msg>,
    pub player: Player,
    pub view: View,
    pub source: Source,
    pub items: Vec<Item>,
    pub collections: Vec<String>,
    pub tags: Vec<String>,
    pub inbox_count: usize,
    pub trash_count: usize,
    /// Trash keys from the last "move to trash", for `u` / ⌘Z in the library.
    pub last_trashed: Vec<String>,
    pub confirm: Option<Confirm>,
    pub marquee: Option<Marquee>,
    /// Collection being renamed in the sidebar: (target, text field).
    pub renaming: Option<(RenameTarget, String)>,
    /// Sounds being dragged from the grid (originals), and the sidebar row under the pointer.
    pub dragging: Option<Vec<PathBuf>>,
    pub drop_hover: Option<DropTarget>,
    /// Pending navigation blocked by unsaved changes (shows the save prompt).
    pub leave: Option<Leave>,
    /// The user confirmed quitting; let the window close.
    pub quitting: bool,
    pub overviews: HashMap<PathBuf, Arc<Overview>>,
    pub overviews_pending: HashSet<PathBuf>,
    /// Decoded sounds for library auditioning (most recent last).
    pub audition_cache: VecDeque<(PathBuf, Arc<Audio>)>,
    pub audition_pending: Option<PathBuf>,
    pub editor: Option<Editor>,
    pub presets: Vec<Preset>,
    pub status: String,
    pub new_collection: String,
    pub library_path_input: String,
    pub last_refresh: f64,
    pub last_frame: f64,
    pub themed: bool,
    pub settings: Settings,
    pub settings_file: Option<PathBuf>,
    /// Folder layout being edited on the Settings page (applied with "Apply folders").
    pub layout_draft: Option<Layout>,
    pub importing: bool,
    // ---- keyboard / selection
    pub keys: crate::keys::Keys,
    pub cursor: usize,
    pub cursor_moved: bool,
    pub marks: BTreeSet<PathBuf>,
    pub visual_anchor: Option<usize>,
    /// Screen rects of library tiles from the last frame (index into `items`), for j/k.
    pub tile_rects: Vec<(usize, egui::Rect)>,
    /// Groups over `items` (label, first index, count).
    pub groups: Vec<(String, usize, usize)>,
    pub item_tags: Vec<Vec<String>>,
    pub item_notes: Vec<String>,
    pub search: Option<String>,
    pub help: bool,
    pub completions: Vec<String>,
    pub yank_recipe: Option<Graph>,
    pub yank_node: Option<crate::graph::NodeData>,
    pub ctx: Option<egui::Context>,
    /// A text field had keyboard focus at the end of the last frame.
    pub text_focus: bool,
}

impl HeardApp {
    pub fn new(cfg: AppConfig) -> Self {
        let settings = cfg
            .settings_file
            .as_deref()
            .map(Settings::load)
            .unwrap_or_default();
        let mut app = Self {
            lib: None,
            worker: Worker::new(cfg.sync),
            player: if cfg.sync {
                Player::silent()
            } else {
                Player::open()
            },
            view: View::Library,
            source: Source::Inbox,
            items: vec![],
            collections: vec![],
            tags: vec![],
            inbox_count: 0,
            trash_count: 0,
            last_trashed: vec![],
            confirm: None,
            marquee: None,
            renaming: None,
            dragging: None,
            drop_hover: None,
            leave: None,
            quitting: false,
            overviews: HashMap::new(),
            overviews_pending: HashSet::new(),
            audition_cache: VecDeque::new(),
            audition_pending: None,
            editor: None,
            presets: presets::builtin(),
            status: String::new(),
            new_collection: String::new(),
            library_path_input: String::new(),
            last_refresh: 0.0,
            last_frame: 0.0,
            themed: false,
            settings,
            settings_file: cfg.settings_file,
            layout_draft: None,
            importing: false,
            keys: Default::default(),
            cursor: 0,
            cursor_moved: false,
            marks: BTreeSet::new(),
            visual_anchor: None,
            tile_rects: vec![],
            groups: vec![],
            item_tags: vec![],
            item_notes: vec![],
            search: None,
            help: false,
            completions: vec![],
            yank_recipe: None,
            yank_node: None,
            ctx: None,
            text_focus: false,
        };
        if let Some(e) = &app.player.error {
            app.status = format!("No audio output ({e}) — playback is silent");
        }
        // Startup: an explicit library wins; otherwise reopen the last one only if the user
        // asked for that, else show the picker.
        let startup = cfg.library.or_else(|| {
            app.settings
                .open_last_on_startup
                .then(|| app.settings.last_library().cloned())
                .flatten()
                .filter(|p| p.is_dir())
        });
        if let Some(root) = startup {
            app.set_library(root);
        }
        app
    }

    pub fn from_creation_context(cc: &eframe::CreationContext<'_>, cfg: AppConfig) -> Self {
        theme::apply(&cc.egui_ctx);
        let mut app = Self::new(cfg);
        app.themed = true;
        app
    }

    pub fn save_settings(&mut self) {
        if let Some(p) = &self.settings_file
            && let Err(e) = self.settings.save(p)
        {
            self.status = format!("Couldn't save settings: {e:#}");
        }
    }

    /// The grouping that applies to the current view.
    pub fn group_by(&self) -> crate::settings::GroupBy {
        match self.source {
            Source::AllCollections => self.settings.group_all,
            Source::Trash => crate::settings::GroupBy::Day,
            _ => self.settings.group_by,
        }
    }

    pub fn set_group_by(&mut self, g: crate::settings::GroupBy) {
        match self.source {
            Source::AllCollections => self.settings.group_all = g,
            Source::Trash => {}
            _ => self.settings.group_by = g,
        }
        self.save_settings();
        self.refresh();
    }

    pub fn editor_dirty(&self) -> bool {
        self.editor.as_ref().is_some_and(|e| e.dirty())
    }

    pub fn any_dirty(&self) -> bool {
        self.editor_dirty()
    }

    pub fn set_library(&mut self, root: PathBuf) {
        if self.any_dirty() {
            self.leave = Some(Leave::SetLibrary(root));
            return;
        }
        let layout = self.settings.layout_for(&root);
        match Library::open_with(&root, &layout) {
            Ok(l) => {
                self.player.stop();
                self.lib = Some(l);
                self.editor = None;
                self.layout_draft = None;
                self.overviews.clear();
                self.view = View::Library;
                self.source = Source::Inbox;
                self.settings.touch_recent(&root);
                self.save_settings();
                self.refresh();
                // Start where the sounds are: the Inbox if anything's waiting, else everything.
                if self.inbox_count == 0 && !self.collections.is_empty() {
                    self.source = Source::AllCollections;
                    self.refresh();
                }
                self.status = format!("Library: {}", root.display());
            }
            Err(e) => self.status = format!("Can't open library: {e:#}"),
        }
    }

    /// Close the library and go back to the startup picker.
    pub fn close_library(&mut self) {
        if self.any_dirty() {
            self.leave = Some(Leave::CloseLibrary);
            return;
        }
        self.player.stop();
        self.lib = None;
        self.editor = None;
        self.items.clear();
        self.view = View::Library;
    }

    /// Re-read listings from disk.
    pub fn refresh(&mut self) {
        let Some(lib) = &self.lib else { return };
        self.collections = lib.collections();
        self.tags = lib.all_tags().into_iter().collect();
        let inbox = lib.inbox_items();
        self.inbox_count = inbox.len();
        self.trash_count = lib.trash_count();
        let current = self.items.get(self.cursor).map(|i| i.orig.clone());
        let mut items = match &self.source {
            Source::Inbox => inbox,
            Source::Collection(c) => lib.items(c),
            Source::Tag(t) => lib.items_with_tag(t),
            Source::Trash => lib.trash_items(),
            Source::AllCollections => lib.all_items(),
        };
        if self.source != Source::Trash {
            crate::ui::library_ui::sort_items(&mut items, &self.settings);
        }
        self.groups =
            crate::ui::library_ui::make_groups(&mut items, self.group_by(), &self.settings);
        let scs: Vec<_> = items.iter().map(|i| lib.read_sidecar(i)).collect();
        self.item_tags = scs.iter().map(|s| s.tags.clone()).collect();
        self.item_notes = scs.into_iter().map(|s| s.notes).collect();
        self.items = items;
        // Keep the cursor on the same sound across refreshes.
        if let Some(c) = current
            && let Some(i) = self.items.iter().position(|i| i.orig == c)
        {
            self.cursor = i;
        }
        self.cursor = self.cursor.min(self.items.len().saturating_sub(1));
        self.marks.retain(|m| m.exists());
        self.presets = presets::builtin();
        self.presets.extend(presets::load_user(&lib.presets_dir()));
    }

    // ------------------------------------------------------------- messages

    fn handle_messages(&mut self, ctx: &egui::Context) {
        for m in self.worker.poll() {
            match m {
                Msg::Overview(p, o) => {
                    self.overviews_pending.remove(&p);
                    if let Some(o) = o {
                        self.overviews.insert(p, o);
                    }
                }
                Msg::Loaded(r, res) => {
                    if let Some(ed) = &mut self.editor
                        && ed.item.r == r
                    {
                        match res {
                            Ok((a, info)) => {
                                ed.source = Some(a);
                                ed.info = Some(info);
                                ed.evaluator.lock().unwrap().clear();
                                ed.requested.clear();
                            }
                            Err(e) => ed.load_error = Some(e),
                        }
                    }
                }
                Msg::Preview {
                    generation,
                    preview,
                    ms,
                } => {
                    if let Some(ed) = &mut self.editor
                        && generation >= ed.shown_gen
                        && let Some(p) = preview
                    {
                        ed.shown_gen = generation;
                        ed.render_ms = ms;
                        let len = p.audio.seconds();
                        let first = ed.preview.is_none();
                        let shown_changed = ed.preview.as_ref().is_some_and(|o| o.shown != p.shown);
                        // Keep the playhead going on the fresh render (live tweaking).
                        if self.player.is_playing_key(ed.play_key()) {
                            self.player.swap(p.audio.clone(), ed.play_key());
                        }
                        if first || shown_changed {
                            ed.view.fit();
                        }
                        ed.view.clamp(len);
                        if let Some((a, b)) = ed.selection {
                            ed.selection = (a < len).then_some((a, b.min(len)));
                        }
                        ed.markers.retain(|m| *m < len);
                        ed.playhead = ed.playhead.min(len);
                        ed.preview = Some(p);
                        ed.spectro_gen = 0;
                    }
                }
                Msg::Spectro(generation, img) => {
                    if let Some(ed) = &mut self.editor
                        && generation == ed.shown_gen
                    {
                        ed.spectro_gen = generation;
                        match &mut ed.spectro {
                            Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                            None => {
                                ed.spectro = Some(ctx.load_texture(
                                    "spectro",
                                    img,
                                    egui::TextureOptions::LINEAR,
                                ))
                            }
                        }
                    }
                }
                Msg::Audition(p, res) => {
                    let wanted = self.audition_pending.as_ref() == Some(&p);
                    if wanted {
                        self.audition_pending = None;
                    }
                    match res {
                        Ok(a) => {
                            self.audition_cache.retain(|(q, _)| *q != p);
                            self.audition_cache.push_back((p.clone(), a.clone()));
                            while self.audition_cache.len() > 8 {
                                self.audition_cache.pop_front();
                            }
                            if wanted {
                                self.player.play(a, path_key(&p), 0.0, None, false);
                            }
                        }
                        Err(e) => self.status = format!("Can't play: {e}"),
                    }
                }
                Msg::Exported { r, result } => {
                    match &result {
                        Ok(p) => {
                            self.status = format!("Saved {}", p.display());
                            self.forget_audio(p);
                        }
                        Err(e) => self.status = format!("Render failed: {e}"),
                    }
                    if let Some(ed) = &mut self.editor
                        && ed.item.r == r
                    {
                        ed.exporting = false;
                        if result.is_ok() {
                            ed.stale = false;
                            if let Some(lib) = &self.lib
                                && let Some(it) = lib.item(&r)
                            {
                                ed.item = it;
                            }
                        }
                    }
                    self.refresh();
                }
                Msg::Batch(results) => {
                    let ok = results.iter().filter(|(_, r)| r.is_ok()).count();
                    let errs: Vec<&String> = results
                        .iter()
                        .filter_map(|(_, r)| r.as_ref().err())
                        .collect();
                    self.status = if errs.is_empty() {
                        format!("Rendered {ok} sound(s)")
                    } else {
                        format!("Rendered {ok}, {} failed: {}", errs.len(), errs[0])
                    };
                    for (r, res) in &results {
                        if let Ok(p) = res {
                            self.forget_audio(p);
                        }
                        if let Some(ed) = &mut self.editor
                            && &ed.item.r == r
                            && res.is_ok()
                        {
                            ed.stale = false;
                        }
                    }
                    self.refresh();
                }
                Msg::FolderImported(res) => {
                    self.importing = false;
                    self.status = match res {
                        Ok(f) => {
                            let mut s = format!(
                                "Imported {} sound(s) into {} collection(s)",
                                f.filed,
                                f.collections.len()
                            );
                            if f.inbox > 0 {
                                s.push_str(&format!(", {} into the Inbox", f.inbox));
                            }
                            if f.skipped > 0 {
                                s.push_str(&format!(" ({} rendered edits skipped)", f.skipped));
                            }
                            s
                        }
                        Err(e) => format!("Import failed: {e}"),
                    };
                    self.refresh();
                }
            }
        }
    }

    /// Drop cached overview / audition audio for a file that changed on disk.
    pub fn forget_audio(&mut self, p: &std::path::Path) {
        self.overviews.remove(p);
        self.audition_cache.retain(|(q, _)| q != p);
    }

    /// Waveform overview for a library tile (loaded in the background).
    pub fn overview(
        &mut self,
        ctx: &egui::Context,
        path: &std::path::Path,
    ) -> Option<Arc<Overview>> {
        if let Some(o) = self.overviews.get(path) {
            return Some(o.clone());
        }
        if self.overviews_pending.insert(path.to_path_buf()) {
            let p = path.to_path_buf();
            let cache = self.settings.thumb_dir();
            self.worker.spawn_pooled(ctx, move || {
                let o = crate::io::load_overview(&p, cache.as_deref())
                    .ok()
                    .map(Arc::new);
                Msg::Overview(p, o)
            });
        }
        None
    }

    // ------------------------------------------------------------- playback

    /// Library: play / stop the sound under the cursor (its edit if rendered).
    pub fn audition_toggle(&mut self, ctx: &egui::Context, i: usize) {
        let Some(it) = self.items.get(i) else { return };
        let p = it.display_path().to_path_buf();
        if self.player.is_playing_key(path_key(&p)) {
            self.player.stop();
            return;
        }
        self.audition_path(ctx, p);
    }

    pub fn audition_path(&mut self, ctx: &egui::Context, p: PathBuf) {
        if let Some((_, a)) = self.audition_cache.iter().find(|(q, _)| *q == p) {
            self.player.play(a.clone(), path_key(&p), 0.0, None, false);
            return;
        }
        self.player.stop();
        self.audition_pending = Some(p.clone());
        self.worker.spawn(ctx, move || {
            let r = crate::io::load(&p)
                .map(|(a, _)| Arc::new(a))
                .map_err(|e| format!("{e:#}"));
            Msg::Audition(p, r)
        });
    }

    /// Editor: play / stop. From the selection (looping if loop is on), else from the playhead.
    pub fn editor_play_toggle(&mut self) {
        let looping = self.settings.loop_play;
        let Some(ed) = &mut self.editor else { return };
        let key = ed.play_key();
        if self.player.is_playing_key(key) {
            ed.playhead = self.player.position();
            self.player.stop();
            return;
        }
        let Some(p) = &ed.preview else { return };
        let len = p.audio.seconds();
        let (from, to) = match ed.selection {
            Some((a, b)) if b - a > 0.005 => (a, Some(b)),
            _ => {
                let from = if ed.playhead >= len - 0.01 {
                    0.0
                } else {
                    ed.playhead
                };
                (from, None)
            }
        };
        self.player.play(p.audio.clone(), key, from, to, looping);
    }

    pub fn editor_playing(&self) -> bool {
        self.editor
            .as_ref()
            .is_some_and(|ed| self.player.is_playing_key(ed.play_key()))
    }

    // ------------------------------------------------------------- editor lifecycle

    pub fn open_item(&mut self, ctx: &egui::Context, item: Item) {
        if item.is_trashed() {
            self.status = format!("{} is in the trash — restore it (r) to edit", item.title());
            return;
        }
        if self
            .editor
            .as_ref()
            .is_some_and(|e| e.dirty() && e.item.orig != item.orig)
        {
            self.leave = Some(Leave::OpenItem(item));
            return;
        }
        let Some(lib) = &self.lib else { return };
        self.player.stop();
        let sc = lib.read_sidecar(&item);
        let name = match &item.r {
            ItemRef::Inbox(_) => lib.planned_name(&item, &sc),
            _ => item.title(),
        };
        let doc = Doc {
            graph: sc.graph,
            tags: sc.tags,
            notes: sc.notes,
        };
        let key = doc.key();
        let collection_default = match &item.r {
            ItemRef::Filed { collection, .. } => collection.clone(),
            ItemRef::Inbox(_) | ItemRef::Trashed { .. } => match &self.source {
                Source::Collection(c) => c.clone(),
                _ => self
                    .editor
                    .as_ref()
                    .map(|e| e.file_target.clone())
                    .filter(|c| self.collections.contains(c))
                    .or_else(|| self.collections.first().cloned())
                    .unwrap_or_default(),
            },
        };
        // A filed sound without a recipe is a finished original: nothing to render.
        let stale = matches!(item.r, ItemRef::Filed { .. })
            && item.edit.is_none()
            && item.sidecar.exists()
            && !graph::is_identity(&doc.graph);
        let ed = Editor {
            history: History::new(doc.clone(), key),
            doc: doc.clone(),
            source: None,
            info: None,
            source_key: path_key(&item.orig),
            load_error: None,
            preview: None,
            spectro: None,
            spectro_gen: 0,
            evaluator: Arc::new(Mutex::new(Evaluator::new(true))),
            latest_gen: Arc::new(AtomicU64::new(0)),
            requested: String::new(),
            shown_gen: 0,
            render_ms: 0.0,
            patch: self.editor.as_ref().map(|e| e.patch).unwrap_or(false),
            tool: Tool::None,
            compare: false,
            selected: None,
            roll: 1,
            tag_input: String::new(),
            name_input: name,
            file_target: collection_default,
            preset_pick: 0,
            preset_name: String::new(),
            stale,
            exporting: false,
            view: TimeView::default(),
            selection: None,
            playhead: 0.0,
            markers: vec![],
            drag: None,
            param_cursor: 0,
            cursor_moved: false,
            last_adjust: None,
            saved: doc.clone(),
            item: item.clone(),
        };
        self.editor = Some(ed);
        self.view = View::Edit;
        let r = item.r.clone();
        let orig = item.orig.clone();
        self.worker.spawn(ctx, move || {
            let res = crate::io::load(&orig)
                .map(|(a, i)| (Arc::new(a), i))
                .map_err(|e| format!("{e:#}"));
            Msg::Loaded(r, res)
        });
    }

    /// Write the open sound's recipe, tags, notes (and an Inbox sound's planned name).
    pub fn save_editor_sidecar(&mut self) {
        let (Some(lib), Some(ed)) = (&self.lib, &mut self.editor) else {
            return;
        };
        let mut sc = lib.read_sidecar(&ed.item);
        sc.graph = ed.doc.graph.clone();
        sc.tags = ed.doc.tags.clone();
        sc.notes = ed.doc.notes.clone();
        if matches!(ed.item.r, ItemRef::Inbox(_)) {
            sc.name = crate::library::clean_name(&ed.name_input);
        }
        let saved = match lib.write_sidecar(&ed.item, &sc) {
            Ok(()) => {
                ed.saved = ed.doc.clone();
                self.tags = lib.all_tags().into_iter().collect();
                Some((
                    format!("edits of {}", ed.item.title()),
                    ed.doc.graph.clone(),
                ))
            }
            Err(e) => {
                self.status = format!("Couldn't save recipe: {e:#}");
                None
            }
        };
        if let Some((name, g)) = saved {
            self.remember_recipe(name, g);
        }
    }

    /// Remember a recipe for the editor's "Apply last" button (persisted in settings).
    pub fn remember_recipe(&mut self, name: String, graph: Graph) {
        self.settings.last_recipe = Some(crate::settings::LastRecipe { name, graph });
        self.save_settings();
    }

    /// "Apply last": the last applied preset / last saved sound's edits onto this sound,
    /// keeping this sound's trim.
    pub fn apply_last(&mut self) {
        let Some(last) = self.settings.last_recipe.clone() else {
            self.status = "Nothing to apply yet — apply a preset or save a sound first".into();
            return;
        };
        if let Some(ed) = &mut self.editor {
            ed.doc.graph = crate::presets::apply(&ed.doc.graph, &last.graph);
            ed.selected = None;
            self.status = format!("Applied last: {}", last.name);
        }
    }

    /// Throw away unsaved edits of the open sound.
    pub fn discard_editor(&mut self) {
        if let Some(ed) = &mut self.editor {
            ed.doc = ed.saved.clone();
            ed.requested.clear();
        }
    }

    /// Rename the open sound (its files, right away).
    pub fn rename_open_sound(&mut self, new: &str) {
        let (Some(lib), Some(ed)) = (self.lib.clone(), self.editor.as_mut()) else {
            return;
        };
        let new = crate::library::clean_name(new);
        if new.is_empty() {
            self.status = "A name can't be empty".into();
            return;
        }
        if matches!(ed.item.r, ItemRef::Trashed { .. }) {
            return;
        }
        match lib.rename_item(&ed.item, &new) {
            Ok(it) => {
                let was_dirty = ed.dirty();
                ed.item = it;
                ed.name_input = new.clone();
                self.status = format!("Renamed to {new}");
                if was_dirty {
                    self.status.push_str(" (edits still unsaved)");
                }
                self.overviews.clear();
                self.refresh();
            }
            Err(e) => {
                self.status = format!("Couldn't rename: {e:#}");
            }
        }
    }

    /// Rename library sounds by cursor (`:name`, `cw`): one sound → that name; several →
    /// `name-01`, `name-02` …
    pub fn rename_selection(&mut self, new: &str) {
        let Some(lib) = self.lib.clone() else { return };
        let items: Vec<Item> = self
            .selection()
            .into_iter()
            .filter_map(|i| self.items.get(i).cloned())
            .collect();
        let many = items.len() > 1;
        let mut n = 0;
        let mut err = None;
        for (k, it) in items.iter().enumerate() {
            let name = if many {
                format!("{}-{:02}", new.trim(), k + 1)
            } else {
                new.to_string()
            };
            match lib.rename_item(it, &name) {
                Ok(newit) => {
                    n += 1;
                    if let Some(ed) = &mut self.editor
                        && ed.item.orig == it.orig
                    {
                        ed.item = newit.clone();
                        ed.name_input = crate::library::clean_name(&name);
                    }
                    if self.marks.remove(&it.orig) {
                        self.marks.insert(newit.orig.clone());
                    }
                }
                Err(e) => err = Some(format!("{e:#}")),
            }
        }
        self.overviews.clear();
        self.status = match err {
            None => format!("Renamed {n} sound(s)"),
            Some(e) => format!("Renamed {n}; {e}"),
        };
        self.refresh();
    }

    /// Copy the open sound into `collection` as a new item with the current recipe, and
    /// render its edit. The open sound itself is left as it is.
    pub fn save_copy(&mut self, ctx: &egui::Context, collection: &str) {
        let (Some(lib), Some(ed)) = (&self.lib, &self.editor) else {
            return;
        };
        let Some(col) = self
            .collections
            .iter()
            .find(|c| c.eq_ignore_ascii_case(collection.trim()))
            .cloned()
        else {
            self.status = format!("No collection {collection}");
            return;
        };
        let mut sc = lib.read_sidecar(&ed.item);
        sc.graph = ed.doc.graph.clone();
        sc.tags = ed.doc.tags.clone();
        sc.notes = ed.doc.notes.clone();
        sc.name = ed.name_input.clone();
        match lib.copy_into(&ed.item, &col, &sc) {
            Ok(item) => {
                let dest = lib.edit_path(&item).unwrap();
                let graph = ed.doc.graph.clone();
                self.render_job(ctx, item.orig.clone(), graph, dest, item.r.clone());
                self.status = format!("Saved a copy as {} — rendering…", item.title());
                self.refresh();
            }
            Err(e) => self.status = format!("Couldn't save a copy: {e:#}"),
        }
    }

    /// Render `orig` through `graph` into `dest` (background), matching the source format.
    fn render_job(
        &mut self,
        ctx: &egui::Context,
        orig: PathBuf,
        graph: Graph,
        dest: PathBuf,
        r: ItemRef,
    ) {
        self.worker.spawn(ctx, move || {
            let res = render_file(&orig, &graph, &dest);
            Msg::Exported {
                r,
                result: res.map_err(|e| format!("{e:#}")),
            }
        });
    }

    /// Move the current Inbox item into `collection`, write the recipe + render edit.wav.
    pub fn file_current(&mut self, ctx: &egui::Context, collection: &str) {
        let (Some(lib), Some(ed)) = (&self.lib, &mut self.editor) else {
            return;
        };
        if !matches!(ed.item.r, ItemRef::Inbox(_)) {
            return;
        }
        let mut sc = lib.read_sidecar(&ed.item);
        sc.tags = ed.doc.tags.clone();
        sc.notes = ed.doc.notes.clone();
        sc.graph = ed.doc.graph.clone();
        sc.name = crate::library::clean_name(&ed.name_input);
        let identity = graph::is_identity(&ed.doc.graph);
        match lib.file_into(&ed.item, collection, &sc) {
            Ok(item) => {
                self.status = format!("Filed into {collection} as {}", item.title());
                ed.saved = ed.doc.clone();
                self.settings.last_recipe = Some(crate::settings::LastRecipe {
                    name: format!("edits of {}", item.title()),
                    graph: ed.doc.graph.clone(),
                });
                ed.name_input = item.title();
                ed.item = item;
                // An untouched recording is filed as it is (nothing to render).
                if identity {
                    ed.stale = false;
                    self.save_editor_sidecar();
                } else {
                    ed.stale = true;
                    self.export_current(ctx);
                }
                self.refresh();
            }
            Err(e) => self.status = format!("Filing failed: {e:#}"),
        }
    }

    /// Render the edit for the current (filed) item in the background.
    pub fn export_current(&mut self, ctx: &egui::Context) {
        self.save_editor_sidecar();
        let (Some(lib), Some(ed)) = (&self.lib, &mut self.editor) else {
            return;
        };
        let Some(dest) = lib.edit_path(&ed.item) else {
            self.status = "File the recording into a collection first".into();
            return;
        };
        ed.exporting = true;
        let graph = ed.doc.graph.clone();
        let orig = ed.item.orig.clone();
        let r = ed.item.r.clone();
        self.status = "Rendering…".into();
        self.render_job(ctx, orig, graph, dest, r);
    }

    pub fn next_inbox_item(&self) -> Option<Item> {
        self.lib.as_ref()?.inbox_items().into_iter().next()
    }

    /// Cut what the editor shows at its markers (or the selection) into new Inbox recordings.
    pub fn split_to_inbox(&mut self, selection_only: bool) {
        let (Some(lib), Some(ed)) = (self.lib.clone(), self.editor.as_ref()) else {
            return;
        };
        let Some(p) = ed.preview.as_ref().filter(|p| p.shown == Shown::Output) else {
            self.status = "Wait for the preview (and leave trim / before mode) first".into();
            return;
        };
        let len = p.audio.seconds();
        let regions: Vec<(f64, f64)> = if selection_only {
            match ed.selection {
                Some(s) if s.1 - s.0 > 0.01 => vec![s],
                _ => {
                    self.status = "Select a region on the waveform first".into();
                    return;
                }
            }
        } else {
            let mut cuts: Vec<f64> = ed
                .markers
                .iter()
                .copied()
                .filter(|m| *m > 0.005 && *m < len - 0.005)
                .collect();
            cuts.sort_by(|a, b| a.total_cmp(b));
            if cuts.is_empty() {
                self.status = "No markers — press , at the playhead, or :markers auto".into();
                return;
            }
            let mut edges = vec![0.0];
            edges.extend(cuts);
            edges.push(len);
            edges
                .windows(2)
                .map(|w| (w[0], w[1]))
                .filter(|(a, b)| b - a > 0.01)
                .collect()
        };
        let base = crate::library::clean_name(&ed.name_input);
        let (bits, float) = ed
            .info
            .as_ref()
            .map(crate::io::export_bits)
            .unwrap_or((24, false));
        let mut n = 0;
        let mut err = None;
        for (k, (a, b)) in regions.iter().enumerate() {
            let mut slice = p
                .audio
                .slice(p.audio.secs_to_frame(*a), p.audio.secs_to_frame(*b));
            crate::ops::clean::edge_fades(&mut slice, 3.0, 5.0);
            let stem = if regions.len() == 1 && selection_only {
                format!("{base}-part")
            } else {
                format!("{base}-{:02}", k + 1)
            };
            let sc = crate::library::Sidecar {
                source_name: format!(
                    "{} · {}–{}",
                    ed.item
                        .orig
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(""),
                    crate::audio::fmt_time(*a),
                    crate::audio::fmt_time(*b)
                ),
                tags: ed.doc.tags.clone(),
                notes: ed.doc.notes.clone(),
                ..Default::default()
            };
            match lib.write_inbox_wav(&stem, &slice, bits, float, &sc) {
                Ok(_) => n += 1,
                Err(e) => err = Some(format!("{e:#}")),
            }
        }
        self.status = match err {
            None => format!("Split into {n} new recording(s) in the Inbox"),
            Some(e) => format!("Split {n}; failed: {e}"),
        };
        self.refresh();
    }

    // ------------------------------------------------------------- trash

    /// Move these sounds to the library's local trash (restorable).
    pub fn trash_items(&mut self, items: Vec<Item>) {
        let Some(lib) = self.lib.clone() else { return };
        let items: Vec<Item> = items.into_iter().filter(|i| !i.is_trashed()).collect();
        if items.is_empty() {
            return;
        }
        self.player.stop();
        let mut keys = vec![];
        let mut err = None;
        for it in &items {
            match lib.trash(it) {
                Ok(k) => keys.push(k),
                Err(e) => err = Some(format!("{}: {e:#}", it.title())),
            }
            self.forget_audio(&it.orig);
            if let Some(e) = &it.edit {
                self.forget_audio(e);
            }
            if self
                .editor
                .as_ref()
                .is_some_and(|ed| ed.item.orig == it.orig)
            {
                self.editor = None;
            }
        }
        self.status = match err {
            None => format!("Moved {} sound(s) to the trash — u to undo", keys.len()),
            Some(e) => format!("Moved {} to the trash; failed: {e}", keys.len()),
        };
        self.last_trashed = keys;
        self.marks.clear();
        self.visual_anchor = None;
        if self.keys.mode == crate::keys::Mode::Visual {
            self.keys.mode = crate::keys::Mode::Normal;
        }
        self.refresh();
    }

    /// Rename a collection and keep everything that points at it working.
    pub fn rename_collection(&mut self, old: &str, new: &str) {
        let Some(lib) = self.lib.clone() else { return };
        let new = match lib.rename_collection(old, new) {
            Ok(n) => n,
            Err(e) => {
                self.status = format!("Couldn't rename {old}: {e:#}");
                return;
            }
        };
        if let Some(ed) = &mut self.editor {
            if let ItemRef::Filed { collection, id } = &ed.item.r
                && collection == old
                && let Some(it) = lib.item(&ItemRef::Filed {
                    collection: new.clone(),
                    id: id.clone(),
                })
            {
                ed.item = it;
            }
            if ed.file_target == old {
                ed.file_target = new.clone();
            }
        }
        if self.source == Source::Collection(old.to_string()) {
            self.source = Source::Collection(new.clone());
        }
        let prefix = lib.root.join(old);
        self.marks = std::mem::take(&mut self.marks)
            .into_iter()
            .map(|m| {
                m.strip_prefix(&prefix)
                    .map(|r| lib.root.join(&new).join(r))
                    .unwrap_or(m)
            })
            .collect();
        self.overviews.retain(|p, _| !p.starts_with(&prefix));
        self.status = format!("Renamed {old} to {new}");
        self.refresh();
    }

    pub fn restore_keys(&mut self, keys: &[String]) -> usize {
        let Some(lib) = self.lib.clone() else {
            return 0;
        };
        let mut n = 0;
        let mut err = None;
        for k in keys {
            match lib.restore(k) {
                Ok(_) => n += 1,
                Err(e) => err = Some(format!("{e:#}")),
            }
        }
        self.status = match err {
            None => format!("Restored {n} sound(s)"),
            Some(e) => format!("Restored {n}; failed: {e}"),
        };
        self.marks.clear();
        self.refresh();
        n
    }

    pub fn restore_last(&mut self) {
        let keys = std::mem::take(&mut self.last_trashed);
        if keys.is_empty() {
            self.status = "Nothing to undo".into();
        } else {
            self.restore_keys(&keys);
        }
    }

    pub fn trash_keys_of(items: &[Item]) -> Vec<String> {
        items
            .iter()
            .filter_map(|i| match &i.r {
                ItemRef::Trashed { key, .. } => Some(key.clone()),
                _ => None,
            })
            .collect()
    }

    /// Run a confirmed permanent delete.
    pub fn run_confirm(&mut self, c: Confirm) {
        let Some(lib) = self.lib.clone() else { return };
        self.player.stop();
        let res = match c {
            Confirm::Purge(keys) => keys.iter().try_fold(0, |n, k| lib.purge(k).map(|_| n + 1)),
            Confirm::EmptyTrash => lib.empty_trash(),
        };
        self.status = match res {
            Ok(n) => format!("Deleted {n} sound(s) permanently"),
            Err(e) => format!("Delete failed: {e:#}"),
        };
        self.last_trashed.clear();
        self.marks.clear();
        self.overviews.retain(|p, _| p.exists());
        self.refresh();
    }

    /// Sidebar rows call this: highlight and remember the row under dragged sounds.
    pub fn drop_zone(&mut self, ui: &Ui, rect: egui::Rect, target: DropTarget) {
        if self.dragging.is_none() {
            return;
        }
        if ui
            .input(|i| i.pointer.hover_pos())
            .is_some_and(|p| rect.contains(p))
        {
            ui.painter()
                .rect_filled(rect, 3.0, theme::ACCENT.gamma_multiply(0.25));
            ui.painter().rect_stroke(
                rect.shrink(1.0),
                3.0,
                egui::Stroke::new(2.0, theme::ACCENT),
                egui::StrokeKind::Inside,
            );
            self.drop_hover = Some(target);
        }
    }

    /// Draw the drag ghost; on release over a sidebar row, move / file / trash the sounds.
    fn finish_drag(&mut self, ctx: &egui::Context) {
        let Some(origs) = self.dragging.clone() else {
            return;
        };
        if ctx.input(|i| i.pointer.primary_down()) {
            if let Some(p) = ctx.input(|i| i.pointer.hover_pos()) {
                let painter = ctx.layer_painter(egui::LayerId::new(
                    egui::Order::Tooltip,
                    egui::Id::new("drag_ghost"),
                ));
                let text = match (&self.drop_hover, origs.len()) {
                    (Some(DropTarget::Collection(c)), n) => format!("Move {n} to {c}"),
                    (Some(DropTarget::Trash), n) => format!("Move {n} to trash"),
                    (None, 1) => "1 sound".into(),
                    (None, n) => format!("{n} sounds"),
                };
                let galley =
                    painter.layout_no_wrap(text, egui::FontId::proportional(12.5), theme::BOX_TEXT);
                let r = egui::Rect::from_min_size(
                    p + egui::vec2(14.0, 10.0),
                    galley.size() + egui::vec2(16.0, 10.0),
                );
                painter.rect_filled(r, 4.0, theme::BOX);
                painter.rect_filled(
                    egui::Rect::from_min_max(r.min, egui::pos2(r.max.x, r.min.y + 3.0)),
                    2.0,
                    theme::ACCENT,
                );
                painter.galley(r.min + egui::vec2(8.0, 6.0), galley, theme::BOX_TEXT);
            }
            ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
            return;
        }
        self.dragging = None;
        let Some(target) = self.drop_hover.take() else {
            return;
        };
        let items: Vec<Item> = self
            .items
            .iter()
            .filter(|i| origs.contains(&i.orig))
            .cloned()
            .collect();
        match target {
            DropTarget::Collection(c) => self.file_items(ctx, items, &c),
            DropTarget::Trash => self.trash_items(items),
        }
    }

    /// Leaving the editor with unsaved changes: stay, and ask.
    fn guard_view(&mut self, prev: View) {
        if prev == self.view || self.leave.is_some() {
            return;
        }
        if prev == View::Edit && self.editor_dirty() {
            self.leave = Some(Leave::View {
                from: prev,
                to: self.view,
            });
            self.view = prev;
        }
    }

    /// Is the pending `leave` waiting on the open sound? (its title)
    fn leave_blocker(&self) -> Option<String> {
        let l = self.leave.as_ref()?;
        let abandons = match l {
            Leave::View { from, .. } => *from == View::Edit,
            _ => true,
        };
        (abandons && self.editor_dirty())
            .then(|| self.editor.as_ref().map(|e| e.item.title()))
            .flatten()
    }

    pub fn leave_choice(&mut self, ctx: &egui::Context, choice: LeaveChoice) {
        let Some(l) = self.leave.clone() else { return };
        match (choice, self.leave_blocker()) {
            (LeaveChoice::Cancel, _) => {
                self.leave = None;
                self.status = "Still editing — ⌘S saves".into();
            }
            (LeaveChoice::Save, Some(_)) => self.save_current(ctx),
            (LeaveChoice::Discard, Some(_)) => self.discard_editor(),
            (_, None) => {}
        }
        if self.leave.is_some() && self.leave_blocker().is_none() {
            self.leave = None;
            self.run_leave(ctx, l);
        }
    }

    fn run_leave(&mut self, ctx: &egui::Context, l: Leave) {
        match l {
            Leave::View { to, .. } => {
                self.view = to;
                if to == View::Library {
                    self.refresh();
                }
            }
            Leave::OpenItem(it) => self.open_item(ctx, it),
            Leave::CloseLibrary => self.close_library(),
            Leave::SetLibrary(p) => self.set_library(p),
            Leave::Quit => {
                self.quitting = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    fn leave_modal(&mut self, ctx: &egui::Context) {
        if self.leave.is_none() {
            return;
        }
        let Some(title) = self.leave_blocker() else {
            // Nothing unsaved any more (saved elsewhere): just go.
            self.leave_choice(ctx, LeaveChoice::Discard);
            return;
        };
        let filed = self
            .editor
            .as_ref()
            .is_some_and(|e| matches!(e.item.r, ItemRef::Filed { .. }));
        let mut choice = None;
        let m = egui::Modal::new(egui::Id::new("leave")).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.label(egui::RichText::new(format!("Save changes to {title}?")).strong().size(15.0).color(theme::TEXT));
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(if filed {
                    "Saving writes the recipe and renders the edit (.edit.wav). Don't save keeps the last saved version."
                } else {
                    "Your changes haven't been saved. Don't save keeps the last saved version."
                })
                .color(theme::TEXT_DIM),
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let save = egui::Button::new(egui::RichText::new("Save").color(egui::Color32::WHITE)).fill(theme::ACCENT);
                if ui.add(save).on_hover_text("Enter").clicked() {
                    choice = Some(LeaveChoice::Save);
                }
                if ui.button("Don't save").on_hover_text("N").clicked() {
                    choice = Some(LeaveChoice::Discard);
                }
                if ui.button("Cancel").on_hover_text("Esc").clicked() {
                    choice = Some(LeaveChoice::Cancel);
                }
            });
        });
        if m.should_close() && choice.is_none() {
            choice = Some(LeaveChoice::Cancel);
        }
        if let Some(c) = choice {
            self.leave_choice(ctx, c);
            ctx.request_repaint();
        }
    }

    fn confirm_modal(&mut self, ctx: &egui::Context) {
        let Some(c) = self.confirm.clone() else {
            return;
        };
        let (title, body) = match &c {
            Confirm::Purge(k) => (
                format!("Delete {} sound(s) forever?", k.len()),
                "The originals, edits and recipes are removed from disk. This can't be undone.",
            ),
            Confirm::EmptyTrash => (
                format!("Empty the trash ({} sounds)?", self.trash_count),
                "Everything in the trash is removed from disk. This can't be undone.",
            ),
        };
        let mut choice = None;
        let m = egui::Modal::new(egui::Id::new("confirm")).show(ctx, |ui| {
            ui.set_width(360.0);
            ui.label(
                egui::RichText::new(title)
                    .strong()
                    .size(15.0)
                    .color(theme::TEXT),
            );
            ui.add_space(4.0);
            ui.label(egui::RichText::new(body).color(theme::TEXT_DIM));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let del = egui::Button::new(
                    egui::RichText::new("Delete forever").color(egui::Color32::WHITE),
                )
                .fill(theme::RED);
                if ui.add(del).on_hover_text("Enter").clicked() {
                    choice = Some(true);
                }
                if ui.button("Cancel").on_hover_text("Esc").clicked() {
                    choice = Some(false);
                }
            });
        });
        if m.should_close() && choice.is_none() {
            choice = Some(false);
        }
        match choice {
            Some(true) => {
                self.confirm = None;
                self.run_confirm(c);
            }
            Some(false) => self.confirm = None,
            None => {}
        }
    }

    // ------------------------------------------------------------- frame

    pub fn save_current(&mut self, ctx: &egui::Context) {
        if let Some(ed) = &self.editor {
            if matches!(ed.item.r, ItemRef::Filed { .. }) {
                self.export_current(ctx);
            } else {
                self.save_editor_sidecar();
                self.status = "Recipe saved (file into a collection to render the edit)".into();
            }
        }
    }

    pub fn undo(&mut self) {
        match self.view {
            View::Edit => {
                if let Some(ed) = &mut self.editor
                    && let Some(d) = ed.history.undo(|d| d.key())
                {
                    ed.doc = d;
                }
            }
            View::Library => self.restore_last(),
            View::Settings => {}
        }
    }

    pub fn redo(&mut self) {
        if self.view == View::Edit
            && let Some(ed) = &mut self.editor
            && let Some(d) = ed.history.redo(|d| d.key())
        {
            ed.doc = d;
        }
    }

    /// Commit undo snapshots.
    fn track_history(&mut self, ctx: &egui::Context) {
        let interacting = ctx.input(|i| i.pointer.any_down()) || self.text_focus;
        if let Some(ed) = &mut self.editor {
            let key = ed.doc.key();
            if ed.history.observe(&ed.doc, key, interacting)
                && matches!(ed.item.r, ItemRef::Filed { .. })
            {
                ed.stale = true;
            }
        }
    }

    fn import_dropped(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if dropped.is_empty() {
            return;
        }
        let (dirs, files): (Vec<PathBuf>, Vec<PathBuf>) =
            dropped.into_iter().partition(|p| p.is_dir());
        for d in dirs {
            self.import_folder(ctx, d);
        }
        if !files.is_empty() {
            self.import(files);
        }
    }

    pub fn import(&mut self, files: Vec<PathBuf>) {
        let Some(lib) = &self.lib else { return };
        match lib.import(&files) {
            Ok(n) => self.status = format!("Imported {n} file(s) into the Inbox"),
            Err(e) => self.status = format!("Import failed: {e:#}"),
        }
        self.source = Source::Inbox;
        self.view = View::Library;
        self.refresh();
    }

    /// Copy a folder in: its sub-folders become collections (sounds filed as they are),
    /// loose files go to the Inbox. Runs in the background.
    pub fn import_folder(&mut self, ctx: &egui::Context, folder: PathBuf) {
        let Some(lib) = self.lib.clone() else { return };
        if lib.root.starts_with(&folder) || folder.starts_with(&lib.root) {
            self.status = "That folder is (inside) this library — pick a folder outside it".into();
            return;
        }
        self.importing = true;
        self.status = format!("Importing {}…", folder.display());
        self.view = View::Library;
        self.worker.spawn(ctx, move || {
            Msg::FolderImported(lib.import_folder(&folder).map_err(|e| format!("{e:#}")))
        });
        if self.source == Source::Inbox && self.inbox_count == 0 {
            self.source = Source::AllCollections;
        }
    }

    pub fn frame(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        if self.ctx.is_none() {
            self.ctx = Some(ctx.clone());
        }
        if !self.themed {
            theme::apply(&ctx);
            self.themed = true;
        }
        let now = ctx.input(|i| i.time);
        self.player
            .tick_silent((now - self.last_frame).clamp(0.0, 0.25));
        self.last_frame = now;
        self.handle_messages(&ctx);
        self.import_dropped(&ctx);
        let prev_view = self.view;
        self.drop_hover = None; // recomputed by the sidebar rows this frame
        // Closing the window with unsaved edits asks first.
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting && self.any_dirty() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.leave = Some(Leave::Quit);
        }
        if self.confirm.is_some() || self.leave.is_some() {
            // A dialog is open: Enter = confirm / save, Esc = cancel, N = don't save.
            match modal_key(&ctx) {
                Some(ModalKey::Yes) => {
                    if let Some(c) = self.confirm.take() {
                        self.run_confirm(c);
                    } else {
                        self.leave_choice(&ctx, LeaveChoice::Save);
                    }
                }
                Some(ModalKey::No) if self.leave.is_some() => {
                    self.leave_choice(&ctx, LeaveChoice::Discard);
                }
                Some(ModalKey::No) => {}
                Some(ModalKey::Cancel) => {
                    self.confirm = None;
                    self.leave_choice(&ctx, LeaveChoice::Cancel);
                }
                None => {}
            }
        } else {
            let acts = self.capture_keys(&ctx);
            for a in acts {
                self.act(&ctx, a);
            }
        }
        self.guard_view(prev_view);

        // Watch the Inbox (cheap directory listing every 2 s).
        if now - self.last_refresh > 2.0 && self.view == View::Library && !self.importing {
            self.last_refresh = now;
            self.refresh();
        }
        if !self.worker.is_sync() {
            if self.player.is_playing() {
                ctx.request_repaint(); // moving playhead
            } else {
                ctx.request_repaint_after(std::time::Duration::from_secs(2));
            }
        }

        egui::Panel::top("toolbar")
            .exact_size(crate::ui::shell::TOOLBAR_H)
            .frame(
                egui::Frame::new()
                    .fill(theme::TOOLBAR)
                    .inner_margin(egui::Margin::symmetric(8, 0)),
            )
            .show(ui, |ui| self.toolbar(ui));
        self.guard_view(prev_view);
        egui::Panel::bottom("status")
            .exact_size(crate::ui::editor::BAR_H)
            .frame(
                egui::Frame::new()
                    .fill(theme::TOOLBAR)
                    .inner_margin(egui::Margin::symmetric(0, 0)),
            )
            .show(ui, |ui| self.statusline(ui));

        if self.lib.is_none() {
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(theme::CANVAS))
                .show(ui, |ui| self.startup_picker(ui));
            self.help_overlay(&ctx);
            return;
        }

        let compact = self.settings.sidebar_compact;
        let mut side = egui::Panel::left(if compact {
            "sidebar_compact"
        } else {
            "sidebar"
        })
        .resizable(!compact)
        .default_size(210.0)
        .show_separator_line(true)
        .frame(
            egui::Frame::new()
                .fill(if compact { theme::RAIL } else { theme::SIDEBAR })
                .inner_margin(egui::Margin::symmetric(
                    if compact { 4 } else { 0 },
                    if compact { 6 } else { 0 },
                )),
        );
        if compact {
            side = side.exact_size(crate::ui::shell::COMPACT_W);
        }
        side.show(ui, |ui| {
            if compact {
                self.sidebar_compact(ui)
            } else {
                self.sidebar(ui)
            }
        });

        match self.view {
            View::Library => {
                egui::CentralPanel::default()
                    .frame(
                        egui::Frame::new()
                            .fill(theme::CANVAS)
                            .inner_margin(egui::Margin::same(14)),
                    )
                    .show(ui, |ui| self.library_grid(ui));
            }
            View::Edit => self.edit_view(ui),
            View::Settings => {
                egui::CentralPanel::default()
                    .frame(
                        egui::Frame::new()
                            .fill(theme::CANVAS)
                            .inner_margin(egui::Margin::same(16)),
                    )
                    .show(ui, |ui| self.settings_view(ui));
            }
        }
        self.finish_drag(&ctx);
        self.help_overlay(&ctx);
        self.confirm_modal(&ctx);
        self.guard_view(prev_view);
        self.leave_modal(&ctx);
        self.text_focus = ctx.text_edit_focused();

        self.track_history(&ctx);
        if self.worker.has_queued() {
            ctx.request_repaint();
        }
    }
}

impl eframe::App for HeardApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.frame(ui);
    }

    fn on_exit(&mut self) {
        self.player.stop();
        self.save_settings();
    }
}

/// Decode → graph → WAV in the source's format (rate and channels as rendered, 24-bit, or
/// 32-bit float for float sources).
pub fn render_file(
    orig: &std::path::Path,
    graph: &Graph,
    dest: &std::path::Path,
) -> anyhow::Result<PathBuf> {
    let (src, info) = crate::io::load(orig)?;
    let out = graph::render_full(graph, src)
        .ok_or_else(|| anyhow::anyhow!("graph has no connected output"))?;
    if out.frames() == 0 {
        anyhow::bail!("the recipe produces silence of zero length (check Trim)");
    }
    let (bits, float) = crate::io::export_bits(&info);
    crate::io::save_wav(&out, dest, bits, float)?;
    Ok(dest.to_path_buf())
}

pub fn path_key(p: &std::path::Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    p.hash(&mut h);
    if let Ok(m) = std::fs::metadata(p) {
        m.len().hash(&mut h);
        m.modified().ok().hash(&mut h);
    }
    h.finish()
}

pub fn gen_next(a: &AtomicU64) -> u64 {
    a.fetch_add(1, Ordering::SeqCst) + 1
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaveChoice {
    Save,
    Discard,
    Cancel,
}

enum ModalKey {
    Yes,
    No,
    Cancel,
}

/// Keys for an open dialog (consumed so nothing else reacts).
fn modal_key(ctx: &egui::Context) -> Option<ModalKey> {
    ctx.input_mut(|i| {
        let k = if i.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
            Some(ModalKey::Yes)
        } else if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
            Some(ModalKey::Cancel)
        } else if i.consume_key(egui::Modifiers::NONE, egui::Key::N)
            || i.consume_key(egui::Modifiers::NONE, egui::Key::D)
        {
            Some(ModalKey::No)
        } else {
            None
        };
        i.events
            .retain(|e| !matches!(e, egui::Event::Text(_) | egui::Event::Key { .. }));
        k
    })
}
