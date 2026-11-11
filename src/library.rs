//! Library on disk: one folder per collection plus an Inbox. Sounds keep their own names; the
//! rendered edit and the recipe sit next to the original. The root and the Inbox / presets
//! folders come from user settings ([`crate::settings::Layout`]); the tree shows the defaults.
//!
//! ```text
//! library/
//!   Inbox/                              recordings to process (any folder, e.g. a recorder's SD card)
//!   .heard/inbox-edits/                 in-progress recipes for Inbox items
//!   Drone/
//!     generator_royal_hospital.wav      original (never modified)
//!     generator_royal_hospital.edit.wav rendered edit (WAV, source rate / channels, 24-bit)
//!     generator_royal_hospital.json     sidecar: recipe graph, tags, notes
//!   _presets/                           user presets
//!   .heard/trash/<stamp>/               local trash: moved files + meta.json (restore puts them back)
//! ```

use crate::graph::{self, Graph};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::settings::Layout;

/// App-private state inside the library root (hidden).
const PRIVATE: &str = ".heard";
/// Suffix of rendered edits.
pub const EDIT_SUFFIX: &str = ".edit.wav";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sidecar {
    #[serde(default = "one")]
    pub version: u32,
    /// The file name the sound arrived with (e.g. `ZOOM0042.WAV`).
    #[serde(default)]
    pub source_name: String,
    /// Inbox only: the name to file it under (filed sounds are named by their file).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Free text: where / when / what it is.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
    #[serde(default = "graph::default_graph")]
    pub graph: Graph,
    #[serde(default)]
    pub modified: u64,
}

fn one() -> u32 {
    1
}

impl Default for Sidecar {
    fn default() -> Self {
        Self {
            version: 1,
            source_name: String::new(),
            name: String::new(),
            tags: vec![],
            notes: String::new(),
            graph: graph::default_graph(),
            modified: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ItemRef {
    /// File name in the Inbox.
    Inbox(String),
    /// `id` = the sound's name (file stem).
    Filed { collection: String, id: String },
    /// In the local trash: folder name inside `.heard/trash` + the title it had.
    Trashed { key: String, title: String },
}

/// Where a trashed sound came from (stored in the trash entry's `meta.json`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum TrashFrom {
    Inbox { name: String },
    Filed { collection: String, id: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrashMeta {
    pub from: TrashFrom,
    /// Unix seconds.
    pub deleted: u64,
    /// Original file extension.
    pub ext: String,
}

const TRASH_ORIG: &str = "orig";
const TRASH_EDIT: &str = "edit.wav";
const TRASH_SIDECAR: &str = "sidecar.json";

#[derive(Clone, Debug)]
pub struct Item {
    pub r: ItemRef,
    pub orig: PathBuf,
    pub edit: Option<PathBuf>,
    pub sidecar: PathBuf,
    /// File modification time of the original (unix seconds).
    pub modified: i64,
}

fn mtime(p: &Path) -> i64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl Item {
    pub fn title(&self) -> String {
        match &self.r {
            ItemRef::Inbox(name) => name.clone(),
            ItemRef::Filed { id, .. } => id.clone(),
            ItemRef::Trashed { title, .. } => title.clone(),
        }
    }

    pub fn is_trashed(&self) -> bool {
        matches!(self.r, ItemRef::Trashed { .. })
    }

    /// What to play / draw: the rendered edit if present, else the original.
    pub fn display_path(&self) -> &Path {
        self.edit.as_deref().unwrap_or(&self.orig)
    }

    pub fn collection(&self) -> Option<&str> {
        match &self.r {
            ItemRef::Filed { collection, .. } => Some(collection),
            _ => None,
        }
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A safe file stem from a typed name: no path separators or leading dots, spaces kept.
pub fn clean_name(name: &str) -> String {
    let s: String = name
        .trim()
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let s = s.trim_start_matches('.').trim().to_string();
    // Don't let a typed name look like a rendered edit.
    s.strip_suffix(".edit").map(String::from).unwrap_or(s)
}

fn stem_of(p: &Path) -> String {
    let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("sound");
    match n.rsplit_once('.') {
        Some((s, _)) if !s.is_empty() => s.to_string(),
        _ => n.to_string(),
    }
}

fn ext_of(p: &Path) -> String {
    p.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("wav")
        .to_ascii_lowercase()
}

fn is_edit_file(p: &Path) -> bool {
    p.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.to_ascii_lowercase().ends_with(EDIT_SUFFIX) || n.contains(".tmp."))
}

#[derive(Clone, Debug)]
pub struct Library {
    pub root: PathBuf,
    pub inbox: PathBuf,
    pub presets: PathBuf,
}

/// What an "Import folder…" did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FolderImport {
    pub inbox: usize,
    pub filed: usize,
    pub collections: Vec<String>,
    pub skipped: usize,
}

impl Library {
    /// Open with the default layout.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        Self::open_with(root, &Layout::default())
    }

    pub fn open_with(root: impl Into<PathBuf>, layout: &Layout) -> Result<Self> {
        let root: PathBuf = root.into();
        if root.as_os_str().is_empty() {
            bail!("no library folder given");
        }
        let lib = Self {
            inbox: Layout::resolve(&root, &layout.inbox),
            presets: Layout::resolve(&root, &layout.presets),
            root,
        };
        for d in [&lib.root, &lib.inbox, &lib.presets] {
            std::fs::create_dir_all(d).with_context(|| format!("create {}", d.display()))?;
        }
        Ok(lib)
    }

    pub fn inbox_dir(&self) -> PathBuf {
        self.inbox.clone()
    }
    pub fn presets_dir(&self) -> PathBuf {
        self.presets.clone()
    }

    /// Folders inside the root that are not collections.
    fn is_special(&self, dir: &Path) -> bool {
        let same = |a: &Path, b: &Path| {
            a == b
                || (a.canonicalize().ok().is_some()
                    && a.canonicalize().ok() == b.canonicalize().ok())
        };
        [&self.inbox, &self.presets].iter().any(|s| same(dir, s))
    }

    pub fn collections(&self) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(&self.root)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_dir() && !self.is_special(&e.path()))
            .filter_map(|e| e.file_name().to_str().map(String::from))
            .filter(|n| !n.starts_with('_') && !n.starts_with('.'))
            .collect();
        v.sort_by_key(|a| a.to_lowercase());
        v
    }

    fn check_collection_name(&self, name: &str) -> Result<()> {
        if name.is_empty()
            || name.starts_with(['_', '.'])
            || name.contains(['/', '\\', ':'])
            || self.is_special(&self.root.join(name))
        {
            bail!("invalid collection name");
        }
        Ok(())
    }

    pub fn create_collection(&self, name: &str) -> Result<String> {
        let name = name.trim();
        self.check_collection_name(name)?;
        std::fs::create_dir_all(self.root.join(name))?;
        Ok(name.to_string())
    }

    /// Rename a collection folder. Trashed sounds that came from it restore into the new name.
    pub fn rename_collection(&self, old: &str, new: &str) -> Result<String> {
        let new = new.trim();
        self.check_collection_name(new)?;
        let from = self.root.join(old);
        if old.is_empty() || !from.is_dir() || self.is_special(&from) {
            bail!("no collection {old}");
        }
        if new == old {
            return Ok(new.to_string());
        }
        let to = self.root.join(new);
        if to.exists() && !new.eq_ignore_ascii_case(old) {
            bail!("{new} already exists");
        }
        std::fs::rename(&from, &to)?;
        for e in std::fs::read_dir(self.trash_dir())
            .into_iter()
            .flatten()
            .flatten()
        {
            let Some(key) = e.file_name().to_str().map(String::from) else {
                continue;
            };
            if let Some(mut m) = self.trash_meta(&key)
                && let TrashFrom::Filed { collection, .. } = &mut m.from
                && collection == old
            {
                *collection = new.to_string();
                std::fs::write(
                    e.path().join("meta.json"),
                    serde_json::to_string_pretty(&m)?,
                )?;
            }
        }
        Ok(new.to_string())
    }

    fn inbox_sidecar(&self, name: &str) -> PathBuf {
        self.root
            .join(PRIVATE)
            .join("inbox-edits")
            .join(format!("{name}.json"))
    }

    pub fn inbox_items(&self) -> Vec<Item> {
        let mut v: Vec<Item> = std::fs::read_dir(self.inbox_dir())
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file() && crate::io::is_audio(p) && !is_edit_file(p))
            .filter_map(|p| {
                let name = p.file_name()?.to_str()?.to_string();
                Some(Item {
                    sidecar: self.inbox_sidecar(&name),
                    r: ItemRef::Inbox(name),
                    modified: mtime(&p),
                    orig: p,
                    edit: None,
                })
            })
            .collect();
        v.sort_by(|a, b| a.orig.cmp(&b.orig));
        v
    }

    fn filed_item(&self, collection: &str, orig: PathBuf) -> Item {
        let dir = self.root.join(collection);
        let id = stem_of(&orig);
        let edit = dir.join(format!("{id}{EDIT_SUFFIX}"));
        Item {
            r: ItemRef::Filed {
                collection: collection.into(),
                id: id.clone(),
            },
            sidecar: dir.join(format!("{id}.json")),
            edit: edit.exists().then_some(edit),
            modified: mtime(&orig),
            orig,
        }
    }

    pub fn items(&self, collection: &str) -> Vec<Item> {
        let dir = self.root.join(collection);
        let mut v: Vec<Item> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file() && crate::io::is_audio(p) && !is_edit_file(p))
            .map(|p| self.filed_item(collection, p))
            .collect();
        v.sort_by(|a, b| a.orig.cmp(&b.orig));
        v
    }

    pub fn all_items(&self) -> Vec<Item> {
        self.collections()
            .iter()
            .flat_map(|c| self.items(c))
            .collect()
    }

    pub fn item(&self, r: &ItemRef) -> Option<Item> {
        match r {
            ItemRef::Inbox(name) => self
                .inbox_items()
                .into_iter()
                .find(|i| matches!(&i.r, ItemRef::Inbox(n) if n == name)),
            ItemRef::Filed { collection, .. } => {
                self.items(collection).into_iter().find(|i| &i.r == r)
            }
            ItemRef::Trashed { .. } => self.trash_items().into_iter().find(|i| &i.r == r),
        }
    }

    pub fn read_sidecar(&self, item: &Item) -> Sidecar {
        std::fs::read_to_string(&item.sidecar)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| Sidecar {
                source_name: item
                    .orig
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default()
                    .into(),
                ..Default::default()
            })
    }

    pub fn write_sidecar(&self, item: &Item, sc: &Sidecar) -> Result<()> {
        if let Some(d) = item.sidecar.parent() {
            std::fs::create_dir_all(d)?;
        }
        let mut sc = sc.clone();
        sc.modified = now();
        let tmp = item.sidecar.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&sc)?)?;
        std::fs::rename(tmp, &item.sidecar)?;
        Ok(())
    }

    /// Is `stem` taken in `dir` (by an original, an edit or a recipe)?
    fn stem_taken(dir: &Path, stem: &str) -> bool {
        let lower = stem.to_lowercase();
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().to_str().map(|s| s.to_lowercase()))
            .any(|n| {
                n == format!("{lower}.json")
                    || n == format!("{lower}{EDIT_SUFFIX}")
                    || n.rsplit_once('.').is_some_and(|(s, _)| s == lower)
            })
    }

    /// `stem`, or `stem-2`, `stem-3` … if a sound by that name is already in `dir`.
    pub fn free_stem(dir: &Path, stem: &str) -> String {
        let stem = if stem.trim().is_empty() {
            "sound".to_string()
        } else {
            stem.to_string()
        };
        if !Self::stem_taken(dir, &stem) {
            return stem;
        }
        (2..)
            .map(|k| format!("{stem}-{k}"))
            .find(|s| !Self::stem_taken(dir, s))
            .unwrap()
    }

    /// The name an Inbox recording will be filed under (its planned name, or its file stem).
    pub fn planned_name(&self, item: &Item, sc: &Sidecar) -> String {
        let n = clean_name(&sc.name);
        if n.is_empty() { stem_of(&item.orig) } else { n }
    }

    /// Move an Inbox recording into a collection, named by `sc.name` (or its own file name).
    /// Returns the new item (without an edit; the caller renders it).
    pub fn file_into(&self, inbox_item: &Item, collection: &str, sc: &Sidecar) -> Result<Item> {
        let ItemRef::Inbox(name) = &inbox_item.r else {
            bail!("not an inbox item")
        };
        let dir = self.root.join(collection);
        if !dir.is_dir() {
            bail!("collection {collection} does not exist");
        }
        let stem = Self::free_stem(&dir, &self.planned_name(inbox_item, sc));
        let orig = dir.join(format!("{stem}.{}", ext_of(&inbox_item.orig)));
        if orig.exists() {
            bail!("{} already exists", orig.display());
        }
        move_file(&inbox_item.orig, &orig)?;
        let item = self.filed_item(collection, orig);
        let mut sc = sc.clone();
        if sc.source_name.is_empty() {
            sc.source_name = name.clone();
        }
        sc.name.clear();
        self.write_sidecar(&item, &sc)?;
        let _ = std::fs::remove_file(&inbox_item.sidecar);
        Ok(item)
    }

    /// Move a filed sound (original, rendered edit, recipe) into another collection, keeping
    /// its name (a `-2` suffix if that name is taken there).
    pub fn move_to(&self, item: &Item, collection: &str) -> Result<Item> {
        let ItemRef::Filed {
            collection: from,
            id,
        } = &item.r
        else {
            bail!("only filed sounds can be moved; file Inbox recordings instead")
        };
        if from == collection {
            bail!("already in {collection}");
        }
        let dir = self.root.join(collection);
        if !dir.is_dir() {
            bail!("collection {collection} does not exist");
        }
        let stem = Self::free_stem(&dir, id);
        self.move_files(item, &dir, &stem)?;
        Ok(self.filed_item(
            collection,
            dir.join(format!("{stem}.{}", ext_of(&item.orig))),
        ))
    }

    /// Move / rename the three files of a filed sound.
    fn move_files(&self, item: &Item, dir: &Path, stem: &str) -> Result<()> {
        let orig = dir.join(format!("{stem}.{}", ext_of(&item.orig)));
        let edit = dir.join(format!("{stem}{EDIT_SUFFIX}"));
        let sidecar = dir.join(format!("{stem}.json"));
        let same = |a: &Path, b: &Path| {
            a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
        };
        if !same(&orig, &item.orig) && orig.exists() {
            bail!("{} already exists", orig.display());
        }
        move_file(&item.orig, &orig)?;
        let old_edit = item
            .edit
            .clone()
            .or_else(|| self.edit_path(item))
            .filter(|p| p.exists());
        if let Some(e) = &old_edit {
            move_file(e, &edit)?;
        }
        if item.sidecar.exists() {
            move_file(&item.sidecar, &sidecar)?;
        }
        Ok(())
    }

    /// Rename a sound: its original, edit and recipe together (Inbox: the recording's file and
    /// its in-progress recipe). Returns the new item.
    pub fn rename_item(&self, item: &Item, new: &str) -> Result<Item> {
        let new = clean_name(new);
        if new.is_empty() {
            bail!("empty name");
        }
        match &item.r {
            ItemRef::Filed { collection, id } => {
                if *id == new {
                    return Ok(item.clone());
                }
                let dir = self.root.join(collection);
                let case_only = id.eq_ignore_ascii_case(&new);
                if !case_only && Self::stem_taken(&dir, &new) {
                    bail!("{new} already exists in {collection}");
                }
                if case_only {
                    // Case-insensitive volumes: go through a temporary name.
                    let tmp = format!("{new}.renaming");
                    self.move_files(item, &dir, &tmp)?;
                    let mid = self.filed_item(
                        collection,
                        dir.join(format!("{tmp}.{}", ext_of(&item.orig))),
                    );
                    self.move_files(&mid, &dir, &new)?;
                } else {
                    self.move_files(item, &dir, &new)?;
                }
                Ok(self.filed_item(
                    collection,
                    dir.join(format!("{new}.{}", ext_of(&item.orig))),
                ))
            }
            ItemRef::Inbox(old) => {
                // The recording's own file is renamed (keeping its extension as it was).
                let ext = item
                    .orig
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("wav")
                    .to_string();
                let name = format!("{new}.{ext}");
                if name == *old {
                    return Ok(item.clone());
                }
                let dest = self.inbox_dir().join(&name);
                if dest.exists() && !name.eq_ignore_ascii_case(old) {
                    bail!("{name} is already in the Inbox");
                }
                if name.eq_ignore_ascii_case(old) {
                    let tmp = self.inbox_dir().join(format!("{name}.renaming"));
                    move_file(&item.orig, &tmp)?;
                    move_file(&tmp, &dest)?;
                } else {
                    move_file(&item.orig, &dest)?;
                }
                let sidecar = self.inbox_sidecar(&name);
                if item.sidecar.exists() {
                    let mut sc = self.read_sidecar(item);
                    sc.name.clear();
                    std::fs::create_dir_all(sidecar.parent().unwrap())?;
                    std::fs::write(&sidecar, serde_json::to_string_pretty(&sc)?)?;
                    let _ = std::fs::remove_file(&item.sidecar);
                }
                Ok(Item {
                    r: ItemRef::Inbox(name),
                    modified: mtime(&dest),
                    orig: dest,
                    edit: None,
                    sidecar,
                })
            }
            ItemRef::Trashed { .. } => bail!("restore it from the trash first"),
        }
    }

    /// Copy a sound into `collection` as a new item (named `<name>-2` … when needed). The
    /// source is left untouched; `sc` becomes the copy's recipe (render its edit separately).
    pub fn copy_into(&self, src: &Item, collection: &str, sc: &Sidecar) -> Result<Item> {
        if src.is_trashed() {
            bail!("restore it from the trash first");
        }
        let dir = self.root.join(collection);
        if !dir.is_dir() {
            bail!("collection {collection} does not exist");
        }
        let base = match &src.r {
            ItemRef::Inbox(_) => self.planned_name(src, sc),
            _ => src.title(),
        };
        let stem = Self::free_stem(&dir, &base);
        let orig = dir.join(format!("{stem}.{}", ext_of(&src.orig)));
        std::fs::copy(&src.orig, &orig)?;
        let item = self.filed_item(collection, orig);
        let mut sc = sc.clone();
        if sc.source_name.is_empty() {
            sc.source_name = src
                .orig
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .into();
        }
        sc.name.clear();
        self.write_sidecar(&item, &sc)?;
        Ok(item)
    }

    pub fn edit_path(&self, item: &Item) -> Option<PathBuf> {
        match &item.r {
            ItemRef::Filed { collection, id } => Some(
                self.root
                    .join(collection)
                    .join(format!("{id}{EDIT_SUFFIX}")),
            ),
            ItemRef::Inbox(_) | ItemRef::Trashed { .. } => None,
        }
    }

    /// Copy external files into the Inbox (never overwrites; adds a numeric suffix).
    pub fn import(&self, files: &[PathBuf]) -> Result<usize> {
        let mut n = 0;
        for f in files
            .iter()
            .filter(|f| f.is_file() && crate::io::is_audio(f))
        {
            let dest = unique_path(
                &self
                    .inbox_dir()
                    .join(f.file_name().context("no file name")?),
            );
            std::fs::copy(f, &dest)?;
            n += 1;
        }
        Ok(n)
    }

    /// Import a whole folder: audio directly inside goes to the Inbox; each sub-folder becomes
    /// a collection (created if needed) and its sounds (from any depth) are filed there as they
    /// are — they count as finished, with no edit to render. Names are kept; clashes get `-2`.
    pub fn import_folder(&self, folder: &Path) -> Result<FolderImport> {
        let mut out = FolderImport::default();
        let mut entries: Vec<PathBuf> = std::fs::read_dir(folder)
            .with_context(|| format!("read {}", folder.display()))?
            .flatten()
            .map(|e| e.path())
            .collect();
        entries.sort();
        let loose: Vec<PathBuf> = entries
            .iter()
            .filter(|p| p.is_file() && crate::io::is_audio(p))
            .cloned()
            .collect();
        out.inbox = self.import(&loose)?;
        for d in entries.iter().filter(|p| p.is_dir()) {
            let Some(name) = d.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name.starts_with('.') || name.starts_with('_') {
                continue;
            }
            let mut files = vec![];
            collect_audio(d, &mut files, 0);
            if files.is_empty() {
                continue;
            }
            // Another library's Inbox (or one named like ours) feeds our Inbox.
            if name.eq_ignore_ascii_case("inbox") {
                let files: Vec<PathBuf> = files.into_iter().filter(|f| !is_edit_file(f)).collect();
                out.inbox += self.import(&files)?;
                continue;
            }
            let Ok(col) = self.create_collection(name) else {
                out.skipped += files.len();
                continue;
            };
            if !out.collections.contains(&col) {
                out.collections.push(col.clone());
            }
            let dir = self.root.join(&col);
            for f in files {
                // A rendered edit next to its original came along with it; skip it.
                if is_edit_file(&f) {
                    out.skipped += 1;
                    continue;
                }
                let stem = Self::free_stem(&dir, &clean_name(&stem_of(&f)));
                let dest = dir.join(format!("{stem}.{}", ext_of(&f)));
                std::fs::copy(&f, &dest)?;
                // Bring a Heard recipe along when the source folder has one.
                let json = f.with_file_name(format!("{}.json", stem_of(&f)));
                if json.exists() {
                    let _ = std::fs::copy(&json, dir.join(format!("{stem}.json")));
                }
                out.filed += 1;
            }
        }
        Ok(out)
    }

    /// Write a new recording into the Inbox (used by Split). Returns its item.
    pub fn write_inbox_wav(
        &self,
        stem: &str,
        a: &crate::audio::Audio,
        bits: u16,
        float: bool,
        sc: &Sidecar,
    ) -> Result<Item> {
        let dest = unique_path(&self.inbox_dir().join(format!("{}.wav", clean_name(stem))));
        crate::io::save_wav(a, &dest, bits, float)?;
        let name = dest
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let item = Item {
            sidecar: self.inbox_sidecar(&name),
            r: ItemRef::Inbox(name),
            modified: mtime(&dest),
            orig: dest,
            edit: None,
        };
        self.write_sidecar(&item, sc)?;
        Ok(item)
    }

    pub fn all_tags(&self) -> BTreeSet<String> {
        self.all_items()
            .iter()
            .chain(self.inbox_items().iter())
            .flat_map(|i| self.read_sidecar(i).tags)
            .collect()
    }

    pub fn items_with_tag(&self, tag: &str) -> Vec<Item> {
        self.inbox_items()
            .into_iter()
            .chain(self.all_items())
            .filter(|i| self.read_sidecar(i).tags.iter().any(|t| t == tag))
            .collect()
    }

    // ------------------------------------------------------------- local trash

    pub fn trash_dir(&self) -> PathBuf {
        self.root.join(PRIVATE).join("trash")
    }

    /// Move a sound (original + rendered edit + recipe) into the library's local trash.
    pub fn trash(&self, item: &Item) -> Result<String> {
        let from = match &item.r {
            ItemRef::Inbox(name) => TrashFrom::Inbox { name: name.clone() },
            ItemRef::Filed { collection, id } => TrashFrom::Filed {
                collection: collection.clone(),
                id: id.clone(),
            },
            ItemRef::Trashed { .. } => bail!("already in the trash"),
        };
        let (key, dir, deleted) = self.new_trash_entry()?;
        let ext = ext_of(&item.orig);
        let meta = TrashMeta {
            from,
            deleted,
            ext: ext.clone(),
        };
        std::fs::write(dir.join("meta.json"), serde_json::to_string_pretty(&meta)?)?;
        move_file(&item.orig, &dir.join(format!("{TRASH_ORIG}.{ext}")))?;
        if let Some(e) = item
            .edit
            .clone()
            .or_else(|| self.edit_path(item))
            .filter(|p| p.exists())
        {
            move_file(&e, &dir.join(TRASH_EDIT))?;
        }
        if item.sidecar.exists() {
            move_file(&item.sidecar, &dir.join(TRASH_SIDECAR))?;
        }
        Ok(key)
    }

    fn new_trash_entry(&self) -> Result<(String, PathBuf, u64)> {
        let deleted = now();
        let base = self.trash_dir();
        std::fs::create_dir_all(&base)?;
        let mut k = 0;
        let (key, dir) = loop {
            let key = format!("{deleted}-{k:03}");
            let dir = base.join(&key);
            if !dir.exists() {
                break (key, dir);
            }
            k += 1;
        };
        std::fs::create_dir_all(&dir)?;
        Ok((key, dir, deleted))
    }

    fn trash_meta(&self, key: &str) -> Option<TrashMeta> {
        serde_json::from_str(
            &std::fs::read_to_string(self.trash_dir().join(key).join("meta.json")).ok()?,
        )
        .ok()
    }

    fn trash_item(&self, key: &str, meta: &TrashMeta) -> Item {
        let dir = self.trash_dir().join(key);
        let title = match &meta.from {
            TrashFrom::Inbox { name } => name.clone(),
            TrashFrom::Filed { id, .. } => id.clone(),
        };
        let edit = dir.join(TRASH_EDIT);
        Item {
            r: ItemRef::Trashed {
                key: key.to_string(),
                title,
            },
            orig: dir.join(format!("{TRASH_ORIG}.{}", meta.ext)),
            edit: edit.exists().then_some(edit),
            sidecar: dir.join(TRASH_SIDECAR),
            modified: meta.deleted as i64,
        }
    }

    /// Where a trashed sound came from (for the list view).
    pub fn trash_origin(&self, key: &str) -> Option<String> {
        Some(match self.trash_meta(key)?.from {
            TrashFrom::Inbox { .. } => "Inbox".into(),
            TrashFrom::Filed { collection, .. } => collection,
        })
    }

    /// Trashed sounds, newest first (`modified` = time of deletion).
    pub fn trash_items(&self) -> Vec<Item> {
        let mut v: Vec<Item> = std::fs::read_dir(self.trash_dir())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let key = e.file_name().to_str()?.to_string();
                let meta = self.trash_meta(&key)?;
                Some(self.trash_item(&key, &meta))
            })
            .filter(|i| i.orig.exists())
            .collect();
        v.sort_by(|a, b| {
            b.modified
                .cmp(&a.modified)
                .then_with(|| b.orig.cmp(&a.orig))
        });
        v
    }

    pub fn trash_count(&self) -> usize {
        std::fs::read_dir(self.trash_dir())
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().join("meta.json").exists())
            .count()
    }

    /// Put a trashed sound back where it came from (a name that's taken meanwhile gets a
    /// suffix; a deleted collection is recreated).
    pub fn restore(&self, key: &str) -> Result<Item> {
        let meta = self.trash_meta(key).context("not in the trash")?;
        let t = self.trash_item(key, &meta);
        let dir = self.trash_dir().join(key);
        let item = match &meta.from {
            TrashFrom::Inbox { name } => {
                std::fs::create_dir_all(&self.inbox)?;
                let dest = unique_path(&self.inbox.join(name));
                move_file(&t.orig, &dest)?;
                let name = dest
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default()
                    .to_string();
                let sidecar = self.inbox_sidecar(&name);
                if t.sidecar.exists() {
                    std::fs::create_dir_all(sidecar.parent().unwrap())?;
                    move_file(&t.sidecar, &sidecar)?;
                }
                Item {
                    r: ItemRef::Inbox(name),
                    modified: mtime(&dest),
                    orig: dest,
                    edit: None,
                    sidecar,
                }
            }
            TrashFrom::Filed { collection, id } => {
                let cdir = self.root.join(collection);
                std::fs::create_dir_all(&cdir)?;
                let stem = Self::free_stem(&cdir, id);
                let orig = cdir.join(format!("{stem}.{}", meta.ext));
                move_file(&t.orig, &orig)?;
                if let Some(e) = &t.edit {
                    move_file(e, &cdir.join(format!("{stem}{EDIT_SUFFIX}")))?;
                }
                if t.sidecar.exists() {
                    move_file(&t.sidecar, &cdir.join(format!("{stem}.json")))?;
                }
                self.filed_item(collection, orig)
            }
        };
        std::fs::remove_dir_all(&dir)?;
        Ok(item)
    }

    /// Permanently delete one trash entry.
    pub fn purge(&self, key: &str) -> Result<()> {
        if key.is_empty() || key.contains(['/', '\\']) || key.starts_with('.') {
            bail!("bad trash key");
        }
        std::fs::remove_dir_all(self.trash_dir().join(key))?;
        Ok(())
    }

    /// Permanently delete everything in the trash. Returns how many sounds were removed.
    pub fn empty_trash(&self) -> Result<usize> {
        let n = self.trash_count();
        let dir = self.trash_dir();
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        Ok(n)
    }
}

fn collect_audio(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 8 {
        return;
    }
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            !p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.'))
        })
        .collect();
    v.sort();
    for p in v {
        if p.is_dir() {
            collect_audio(&p, out, depth + 1);
        } else if crate::io::is_audio(&p) {
            out.push(p);
        }
    }
}

/// `path`, or `stem-1.ext`, `stem-2.ext` … if it exists.
fn unique_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("sound");
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    let dir = path.parent().unwrap_or(Path::new("."));
    (1..)
        .map(|k| dir.join(format!("{stem}-{k}{ext}")))
        .find(|p| !p.exists())
        .unwrap()
}

fn move_file(from: &Path, to: &Path) -> Result<()> {
    if from == to {
        return Ok(());
    }
    if std::fs::rename(from, to).is_err() {
        std::fs::copy(from, to)?;
        std::fs::remove_file(from)?;
    }
    Ok(())
}
