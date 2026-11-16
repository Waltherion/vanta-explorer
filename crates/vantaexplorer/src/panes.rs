//! Pane split tree (1-4 panes, free horizontal/vertical splits) + per-pane tabs.
//! Layout, open paths and active tab persist to ~/.local/state/vantaexplorer/session.json.

use crate::model::{spawn_listing, Listing, ListingState, LoadMsg};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SplitDir {
    Horizontal, // side by side (vertical divider)
    Vertical,   // stacked (horizontal divider)
}

#[derive(Serialize, Deserialize)]
pub enum Node {
    Leaf(usize),
    Split {
        dir: SplitDir,
        ratio: f32,
        a: Box<Node>,
        b: Box<Node>,
    },
}

pub struct Tab {
    pub listing: Listing,
    pub rx: Option<Receiver<LoadMsg>>,
    pub history: Vec<PathBuf>,
    pub hist_pos: usize,
    /// Selected entry names (name-keyed so sorting/refresh does not scramble it).
    pub selection: HashSet<String>,
    pub select_anchor: Option<String>,
    /// The moving end of a Shift range (Shift+arrow, Shift+click), with the anchor it was made
    /// from: once the anchor changes, it no longer counts.
    pub select_cursor: Option<(String, String)>,
    pub generation: u64,
    /// A reload's rows as they arrive, shown only once all of them are in and sorted. Swapping
    /// them in chunk by chunk made a big folder shrink to its first chunk, jump its scroll and
    /// reorder on every refresh.
    incoming: Vec<crate::model::Entry>,
    /// This reload is swapped in whole. Not for a tab with nothing on screen yet, nor for a
    /// search, whose results are worth seeing as they are found.
    swap_whole: bool,
    /// Breadcrumb edit mode: Some(text) while the path field is open.
    pub path_edit: Option<String>,
    /// Whether the open path field has already been given focus. Runtime-only: focus must be
    /// requested exactly ONCE, see the path-edit block in browser.rs.
    pub path_edit_focused: bool,
    pub typeahead: String,
    /// Search subfolders instead of filtering only this folder. Sticky per tab: once ticked
    /// it stays on for the next search, which is why it is a Tab field and not frame state.
    pub recursive: bool,
    /// Ctrl+F: the search bar shows, searching subfolders, before anything has been typed.
    pub search_open: bool,
    /// Inline rename: (original name, edit buffer). Also used right after New Folder.
    pub renaming: Option<(String, String)>,
    /// A single click landed on the name of an already-selected file. Held for a moment
    /// rather than renaming at once, so a double-click can cancel it and open instead:
    /// egui fires `clicked()` on a double-click's first press too.
    pub rename_armed: Option<(String, std::time::Instant)>,
    /// A file to select and scroll to once the listing arrives, by NAME, because the
    /// visible index it will land on does not exist until the directory has streamed in.
    /// Set when Vanta Explorer is launched pointing at a file; cleared by the view that consumes it.
    pub pending_reveal: Option<String>,
    /// Visible-index the view should scroll to reveal (set by keyboard navigation).
    /// Needed because the lists are virtualized: an off-screen row is never rendered, so
    /// egui's scroll_to_rect has nothing to scroll to.
    pub scroll_to: Option<usize>,
}

impl Tab {
    pub fn new(path: PathBuf) -> Self {
        let mut t = Self {
            listing: Listing::new(path.clone(), 0),
            rx: None,
            history: vec![path],
            hist_pos: 0,
            selection: HashSet::new(),
            select_anchor: None,
            select_cursor: None,
            generation: 0,
            incoming: Vec::new(),
            swap_whole: false,
            path_edit: None,
            path_edit_focused: false,
            typeahead: String::new(),
            recursive: false,
            search_open: false,
            renaming: None,
            rename_armed: None,
            scroll_to: None,
            pending_reveal: None,
        };
        t.reload();
        t
    }

    pub fn path(&self) -> &Path {
        &self.listing.path
    }

    pub fn title(&self) -> String {
        if crate::fileops::in_trash(&self.listing.path) {
            return "Trash".into();
        }
        self.listing
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.listing.path.to_string_lossy().into_owned())
    }

    pub fn reload(&mut self) {
        self.generation += 1;
        let sort = self.listing.sort;
        let old = std::mem::take(&mut self.listing.entries);
        self.swap_whole = !old.is_empty() && self.search_query().is_none();
        self.listing = Listing::new(self.listing.path.clone(), self.generation);
        self.listing.sort = sort;
        self.listing.entries = old;
        self.listing.stale = true;
        self.incoming.clear();
        self.rx = Some(spawn_listing(
            self.listing.path.clone(),
            self.generation,
            self.search_query(),
        ));
    }

    pub fn navigate(&mut self, path: PathBuf) {
        if path == self.listing.path {
            return;
        }
        self.history.truncate(self.hist_pos + 1);
        self.history.push(path.clone());
        self.hist_pos = self.history.len() - 1;
        self.open(path);
    }

    /// The recursive-search needle, or None to list the folder normally.
    pub fn search_query(&self) -> Option<String> {
        (self.recursive && !self.typeahead.is_empty()).then(|| self.typeahead.clone())
    }

    fn open(&mut self, path: PathBuf) {
        self.selection.clear();
        self.select_anchor = None;
        self.path_edit = None;
        self.pending_reveal = None;
        // Latent bug: the filter used to survive navigation, so a new folder could open
        // looking half-empty for no visible reason.
        self.typeahead.clear();
        self.generation += 1;
        let sort = self.listing.sort;
        self.listing = Listing::new(path.clone(), self.generation);
        self.listing.sort = sort;
        self.rx = Some(spawn_listing(path, self.generation, self.search_query()));
    }

    pub fn can_back(&self) -> bool {
        self.hist_pos > 0
    }

    pub fn can_forward(&self) -> bool {
        self.hist_pos + 1 < self.history.len()
    }

    pub fn back(&mut self) {
        if self.can_back() {
            self.hist_pos -= 1;
            self.open(self.history[self.hist_pos].clone());
        }
    }

    pub fn forward(&mut self) {
        if self.can_forward() {
            self.hist_pos += 1;
            self.open(self.history[self.hist_pos].clone());
        }
    }

    pub fn up(&mut self) {
        // The trash is a place of its own, not a folder inside ~/.local/share.
        if crate::fileops::in_trash(&self.listing.path) {
            return;
        }
        if let Some(parent) = self.listing.path.parent() {
            self.navigate(parent.to_path_buf());
        }
    }

    /// Drain pending chunks. Returns true when anything arrived (caller renders directly).
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.rx else { return false };
        let mut dirty = false;
        let mut done = false;
        while let Ok(msg) = rx.try_recv() {
            match msg {
                LoadMsg::Chunk(generation, mut chunk) => {
                    if generation == self.generation {
                        if self.listing.stale && self.swap_whole {
                            // A reload: the old rows stay on screen until the new ones are all in.
                            self.incoming.append(&mut chunk);
                        } else {
                            if self.listing.stale {
                                self.listing.entries.clear();
                                self.listing.stale = false;
                            }
                            self.listing.entries.append(&mut chunk);
                            dirty = true;
                        }
                    }
                }
                LoadMsg::Done(generation) => {
                    if generation == self.generation {
                        if self.listing.stale {
                            // Everything is in: one swap, already sorted below, so the list
                            // changes once. An empty `incoming` is a folder that emptied.
                            self.listing.entries = std::mem::take(&mut self.incoming);
                            self.listing.stale = false;
                        }
                        self.listing.state = ListingState::Ready;
                        self.listing.apply_sort();
                        done = true;
                        dirty = true;
                    }
                }
                LoadMsg::Error(generation, e) => {
                    if generation == self.generation {
                        self.incoming.clear();
                        self.listing.state = ListingState::Error;
                        self.listing.error = Some(e);
                        done = true;
                        dirty = true;
                    }
                }
            }
        }
        if done {
            self.rx = None;
        }
        dirty
    }
}

pub struct Pane {
    pub tabs: Vec<Tab>,
    pub active_tab: usize,
    pub show_hidden: bool,
    /// Name and Size column widths, per pane, panes have different widths, so a single
    /// global setting cannot fit them all. Modified takes the remainder.
    pub col_name_w: f32,
    pub col_size_w: f32,
    /// Icon size in points. ctrl+scroll drives this continuously; crossing GRID_THRESHOLD
    /// flips the pane from list to grid, so there is one control rather than a mode switch
    /// plus a size slider.
    pub zoom: f32,
    /// Folders of the tabs closed in this pane, newest last, for Ctrl+Shift+T. Not saved.
    pub closed_tabs: Vec<PathBuf>,
}

impl Pane {
    pub fn new(path: PathBuf) -> Self {
        Self {
            tabs: vec![Tab::new(path)],
            active_tab: 0,
            show_hidden: false,
            col_name_w: DEFAULT_COL_NAME_W,
            col_size_w: DEFAULT_COL_SIZE_W,
            zoom: DEFAULT_ZOOM,
            closed_tabs: Vec::new(),
        }
    }

    /// Closes tab `i` (never the last one) and remembers its folder for Ctrl+Shift+T.
    pub fn close_tab(&mut self, i: usize) {
        if self.tabs.len() <= 1 || i >= self.tabs.len() {
            return;
        }
        let t = self.tabs.remove(i);
        self.closed_tabs.push(t.path().to_path_buf());
        if self.closed_tabs.len() > 20 {
            self.closed_tabs.remove(0);
        }
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        }
    }

    pub fn tab(&mut self) -> &mut Tab {
        &mut self.tabs[self.active_tab]
    }

    pub fn tab_ref(&self) -> &Tab {
        &self.tabs[self.active_tab]
    }

    pub fn is_grid(&self) -> bool {
        self.zoom >= GRID_THRESHOLD
    }
}

pub struct Workspace {
    pub panes: Vec<Pane>,
    pub tree: Node,
    pub active: usize,
    /// Sidebar width in points; dragged by the user, persisted with the layout.
    pub sidebar_width: f32,
    /// The preview panel is WINDOW-level, not per pane: it always shows the ACTIVE pane's
    /// selection, so one panel and one decoded texture serve every pane.
    pub preview_open: bool,
    pub preview_w: f32,
    /// Last frame's pane geometry (index, left, top), written by the layout pass.
    /// Runtime-only; used to cycle panes in VISUAL reading order (see visual_order).
    pub visual_rects: Vec<(usize, f32, f32)>,
}

impl Workspace {
    pub fn single(path: PathBuf) -> Self {
        Self {
            panes: vec![Pane::new(path)],
            tree: Node::Leaf(0),
            active: 0,
            sidebar_width: DEFAULT_SIDEBAR_W,
            preview_open: false,
            preview_w: DEFAULT_PREVIEW_W,
            visual_rects: Vec::new(),
        }
    }

    pub fn active_pane(&mut self) -> &mut Pane {
        let i = self.active.min(self.panes.len() - 1);
        &mut self.panes[i]
    }

    /// Pane indices in VISUAL reading order (left-to-right, top-to-bottom), from the
    /// geometry the layout pass recorded last frame. Tree order is only a fallback (first
    /// frame / stale counts): it follows split nesting, so e.g. a 2x2 grid built as a
    /// vertical split of two horizontal splits would cycle column-wise, not reading order.
    pub fn visual_order(&self) -> Vec<usize> {
        if self.visual_rects.len() == self.panes.len() {
            let mut v = self.visual_rects.clone();
            // Same row when tops are within a few px (splits at equal ratio are exact,
            // but keep a small epsilon so float noise can't split a row in two).
            const ROW_EPS: f32 = 4.0;
            v.sort_by(|a, b| {
                if (a.2 - b.2).abs() > ROW_EPS {
                    a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal)
                } else {
                    a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)
                }
            });
            return v.into_iter().map(|(i, _, _)| i).collect();
        }
        let mut out = Vec::new();
        fn traverse(n: &Node, out: &mut Vec<usize>) {
            match n {
                Node::Leaf(idx) => out.push(*idx),
                Node::Split { a, b, .. } => {
                    traverse(a, out);
                    traverse(b, out);
                }
            }
        }
        traverse(&self.tree, &mut out);
        out
    }

    /// Split pane `idx`; the new pane opens at the same path.
    ///
    /// Placement rule: if the whole layout is a pure strip in one
    /// direction and the requested split runs across it, the split escalates to the root, so
    /// the new pane spans the full width/height instead of quartering one cell. In mixed
    /// layouts the split stays local, which keeps grids (e.g. 2x2) constructible.
    pub fn split(&mut self, idx: usize, dir: SplitDir) {
        let path = self.panes[idx].tab_ref().path().to_path_buf();
        let new_idx = self.panes.len();
        self.panes.push(Pane::new(path));

        let escalate = matches!(strip_dir(&self.tree), Some(d) if d != dir);
        if escalate {
            let old = std::mem::replace(&mut self.tree, Node::Leaf(usize::MAX));
            self.tree = Node::Split {
                dir,
                ratio: 0.5,
                a: Box::new(old),
                b: Box::new(Node::Leaf(new_idx)),
            };
        } else {
            replace_leaf(&mut self.tree, idx, |leaf| Node::Split {
                dir,
                ratio: 0.5,
                a: Box::new(leaf),
                b: Box::new(Node::Leaf(new_idx)),
            });
        }
        self.active = new_idx;
    }

    pub fn close(&mut self, idx: usize) {
        if self.panes.len() <= 1 {
            return;
        }
        remove_leaf(&mut self.tree, idx);
        self.panes.remove(idx);
        // Re-number leaves above the removed index.
        fix_indices(&mut self.tree, idx);
        if self.active >= self.panes.len() {
            self.active = self.panes.len() - 1;
        }
    }

    pub fn poll(&mut self) -> bool {
        let mut dirty = false;
        for pane in &mut self.panes {
            for tab in &mut pane.tabs {
                dirty |= tab.poll();
            }
        }
        dirty
    }
}

/// Some(dir) when every Split in the tree runs the same direction (a pure strip);
/// None for a single leaf or a mixed layout.
fn strip_dir(node: &Node) -> Option<SplitDir> {
    fn walk(node: &Node, acc: &mut Option<SplitDir>) -> bool {
        match node {
            Node::Leaf(_) => true,
            Node::Split { dir, a, b, .. } => {
                match acc {
                    None => *acc = Some(*dir),
                    Some(d) if *d == *dir => {}
                    _ => return false,
                }
                walk(a, acc) && walk(b, acc)
            }
        }
    }
    let mut acc = None;
    if walk(node, &mut acc) {
        acc
    } else {
        None
    }
}

fn replace_leaf(node: &mut Node, target: usize, f: impl FnOnce(Node) -> Node + Copy) {
    match node {
        Node::Leaf(i) if *i == target => {
            let old = std::mem::replace(node, Node::Leaf(usize::MAX));
            *node = f(old);
        }
        Node::Leaf(_) => {}
        Node::Split { a, b, .. } => {
            replace_leaf(a, target, f);
            replace_leaf(b, target, f);
        }
    }
}

fn remove_leaf(node: &mut Node, target: usize) {
    if let Node::Split { a, b, .. } = node {
        if matches!(**a, Node::Leaf(i) if i == target) {
            *node = std::mem::replace(b, Node::Leaf(usize::MAX));
            return;
        }
        if matches!(**b, Node::Leaf(i) if i == target) {
            *node = std::mem::replace(a, Node::Leaf(usize::MAX));
            return;
        }
        remove_leaf(a, target);
        remove_leaf(b, target);
    }
}

fn fix_indices(node: &mut Node, removed: usize) {
    match node {
        Node::Leaf(i) => {
            if *i > removed {
                *i -= 1;
            }
        }
        Node::Split { a, b, .. } => {
            fix_indices(a, removed);
            fix_indices(b, removed);
        }
    }
}

// --- session persistence ---

#[derive(Serialize, Deserialize)]
struct SessionTab {
    history: Vec<String>,
    hist_pos: usize,
}

#[derive(Serialize, Deserialize)]
struct SessionPane {
    #[serde(default)]
    tabs: Vec<SessionTab>,
    paths: Vec<String>,
    active_tab: usize,
    show_hidden: bool,
    #[serde(default = "default_col_name_w")]
    col_name_w: f32,
    #[serde(default = "default_col_size_w")]
    col_size_w: f32,
    #[serde(default = "default_zoom")]
    zoom: f32,
}

pub const DEFAULT_SIDEBAR_W: f32 = 220.0;
/// Icon sizes. Below the threshold a pane is a list; at or above it becomes a grid.
pub const DEFAULT_ZOOM: f32 = 16.0;
pub const MIN_ZOOM: f32 = 14.0;
pub const MAX_ZOOM: f32 = 256.0;
pub const GRID_THRESHOLD: f32 = 40.0;

pub const DEFAULT_PREVIEW_W: f32 = 260.0;
pub const MIN_PREVIEW_W: f32 = 140.0;
/// The list must keep at least this much room, so the preview can never swallow the pane.
pub const MIN_LIST_W: f32 = 180.0;
pub const DEFAULT_COL_NAME_W: f32 = 420.0;
pub const DEFAULT_COL_SIZE_W: f32 = 90.0;

#[derive(Serialize, Deserialize)]
struct Session {
    panes: Vec<SessionPane>,
    tree: Node,
    active: usize,
    #[serde(default = "default_sidebar_w")]
    sidebar_width: f32,
    #[serde(default)]
    preview_open: bool,
    #[serde(default = "default_preview_w")]
    preview_w: f32,
}

fn default_sidebar_w() -> f32 {
    DEFAULT_SIDEBAR_W
}
fn default_col_name_w() -> f32 {
    DEFAULT_COL_NAME_W
}
fn default_col_size_w() -> f32 {
    DEFAULT_COL_SIZE_W
}
fn default_preview_w() -> f32 {
    DEFAULT_PREVIEW_W
}
fn default_zoom() -> f32 {
    DEFAULT_ZOOM
}

fn session_path() -> Option<PathBuf> {
    crate::model::state_dir().map(|d| d.join("session.json"))
}

/// Serialize on the caller's thread, write on another one.
///
/// This runs every 30s from the frame loop, and `create_dir_all` + `write` are blocking disk
/// IO. Doing them inline stalled a frame at a fixed interval, a periodic hitch that had
/// nothing to do with what was on screen, which is why it showed up just as much on a folder
/// with nothing to decode. Building the struct and the JSON is pure CPU and stays here, so the
/// snapshot is still exactly the state of this frame.
/// Saves the session. `wait`: written before returning, for when the app is about to exit; the
/// periodic save writes on a thread instead. Either way the file is written beside the old one
/// and renamed over it, so a crash mid-write never leaves an empty session, and a session that
/// has not changed since the last save is not written at all.
pub fn save(ws: &Workspace, wait: bool) {
    let Some(path) = session_path() else { return };
    let session = Session {
        panes: ws
            .panes
            .iter()
            .map(|p| SessionPane {
                paths: p.tabs.iter().map(|t| t.path().to_string_lossy().into_owned()).collect(),
                tabs: p.tabs.iter().map(|t| {
                    let start = t.history.len().saturating_sub(100);
                    let hist_pos = t.hist_pos.saturating_sub(start);
                    SessionTab {
                        history: t.history.iter().skip(start).map(|p| p.to_string_lossy().into_owned()).collect(),
                        hist_pos,
                    }
                }).collect(),
                active_tab: p.active_tab,
                show_hidden: p.show_hidden,
                col_name_w: p.col_name_w,
                col_size_w: p.col_size_w,
                zoom: p.zoom,
            })
            .collect(),
        tree: clone_tree(&ws.tree),
        active: ws.active,
        sidebar_width: ws.sidebar_width,
        preview_open: ws.preview_open,
        preview_w: ws.preview_w,
    };
    let Ok(json) = serde_json::to_string_pretty(&session) else { return };
    static LAST: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
    {
        let mut last = LAST.lock().unwrap();
        if *last == json {
            return;
        }
        *last = json.clone();
    }
    let write = move || {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Its own name per write: the periodic save may still be writing when the exit save runs.
        static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let tmp = path.with_extension(format!("json.{}.tmp", SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    };
    if wait {
        write();
    } else {
        std::thread::Builder::new().name("vx-session".into()).spawn(write).ok();
    }
}

fn clone_tree(n: &Node) -> Node {
    match n {
        Node::Leaf(i) => Node::Leaf(*i),
        Node::Split { dir, ratio, a, b } => Node::Split {
            dir: *dir,
            ratio: *ratio,
            a: Box::new(clone_tree(a)),
            b: Box::new(clone_tree(b)),
        },
    }
}

pub fn load(fallback: PathBuf) -> Workspace {
    let restore = || -> Option<Workspace> {
        let json = std::fs::read_to_string(session_path()?).ok()?;
        let s: Session = serde_json::from_str(&json).ok()?;
        if s.panes.is_empty() {
            return None;
        }
        let panes: Vec<Pane> = s
            .panes
            .into_iter()
            .map(|sp| {
                let mut tabs: Vec<Tab> = if !sp.tabs.is_empty() {
                    sp.tabs.into_iter().filter_map(|st| {
                        if st.history.is_empty() { return None; }
                        let history: Vec<PathBuf> = st.history.into_iter().map(PathBuf::from).collect();
                        let hist_pos = st.hist_pos.min(history.len().saturating_sub(1));
                        let mut tab = Tab::new(history[hist_pos].clone());
                        tab.history = history;
                        tab.hist_pos = hist_pos;
                        Some(tab)
                    }).collect()
                } else {
                    sp.paths.iter().map(PathBuf::from).filter(|p| p.is_dir()).map(Tab::new).collect()
                };
                if tabs.is_empty() {
                    tabs.push(Tab::new(home_dir()));
                }
                Pane {
                    active_tab: sp.active_tab.min(tabs.len() - 1),
                    tabs,
                    show_hidden: sp.show_hidden,
                    col_name_w: sp.col_name_w.max(60.0),
                    col_size_w: sp.col_size_w.max(40.0),
                    zoom: sp.zoom.clamp(MIN_ZOOM, MAX_ZOOM),
                    closed_tabs: Vec::new(),
                }
            })
            .collect();
        // Validate the tree references exactly the pane indices we have.
        let mut seen = Vec::new();
        collect_leaves(&s.tree, &mut seen);
        seen.sort_unstable();
        if seen != (0..panes.len()).collect::<Vec<_>>() {
            return None;
        }
        Some(Workspace {
            active: s.active.min(panes.len() - 1),
            panes,
            tree: s.tree,
            sidebar_width: s.sidebar_width.clamp(140.0, 600.0),
            preview_open: s.preview_open,
            preview_w: s.preview_w.max(MIN_PREVIEW_W),
            visual_rects: Vec::new(),
        })
    };
    restore().unwrap_or_else(|| Workspace::single(fallback))
}

fn collect_leaves(n: &Node, out: &mut Vec<usize>) {
    match n {
        Node::Leaf(i) => out.push(*i),
        Node::Split { a, b, .. } => {
            collect_leaves(a, out);
            collect_leaves(b, out);
        }
    }
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn visual_order_is_reading_order_not_tree_order() {
        // A 2x2 grid built as a horizontal split of two vertical splits: tree order walks
        // column-wise (TL, BL, TR, BR); the visual order must read row-wise.
        let mut ws = super::Workspace::single(std::env::temp_dir());
        for _ in 0..3 {
            ws.panes.push(super::Pane::new(std::env::temp_dir()));
        }
        ws.visual_rects = vec![
            (0, 0.0, 0.0),     // top-left
            (1, 0.0, 300.0),   // bottom-left
            (2, 500.0, 0.0),   // top-right
            (3, 500.0, 300.0), // bottom-right
        ];
        assert_eq!(ws.visual_order(), vec![0, 2, 1, 3]);
    }

    #[test]
    fn visual_order_tolerates_float_noise_in_rows() {
        let mut ws = super::Workspace::single(std::env::temp_dir());
        ws.panes.push(super::Pane::new(std::env::temp_dir()));
        ws.visual_rects = vec![(0, 500.0, 0.5), (1, 0.0, 0.0)]; // same row, 0.5px apart
        assert_eq!(ws.visual_order(), vec![1, 0]);
    }

    use super::*;

    fn leaves(ws: &Workspace) -> Vec<usize> {
        let mut v = Vec::new();
        collect_leaves(&ws.tree, &mut v);
        v
    }

    #[test]
    fn perpendicular_split_on_pure_strip_escalates_to_root() {
        // A|B side by side, then a top/bottom split -> full-width bottom row.
        let mut ws = Workspace::single(std::env::temp_dir());
        ws.split(0, SplitDir::Horizontal); // A|B
        ws.split(1, SplitDir::Vertical); // a split across a strip escalates to the root
        match &ws.tree {
            Node::Split {
                dir: SplitDir::Vertical,
                a,
                b,
                ..
            } => {
                assert!(matches!(
                    **a,
                    Node::Split {
                        dir: SplitDir::Horizontal,
                        ..
                    }
                ));
                assert!(matches!(**b, Node::Leaf(2)));
            }
            _ => panic!("expected root V split"),
        }
        assert_eq!(leaves(&ws).len(), 3);
    }

    #[test]
    fn mixed_layout_splits_locally_so_grids_work() {
        // (A|B)/C, then split C left/right -> 2x2, NOT a full-height column.
        let mut ws = Workspace::single(std::env::temp_dir());
        ws.split(0, SplitDir::Horizontal);
        ws.split(1, SplitDir::Vertical);
        ws.split(2, SplitDir::Horizontal);
        match &ws.tree {
            Node::Split {
                dir: SplitDir::Vertical,
                b,
                ..
            } => {
                assert!(matches!(
                    **b,
                    Node::Split {
                        dir: SplitDir::Horizontal,
                        ..
                    }
                ));
            }
            _ => panic!("expected root V with H bottom row"),
        }
        assert_eq!(leaves(&ws).len(), 4);
    }

    #[test]
    fn parallel_split_stays_local() {
        let mut ws = Workspace::single(std::env::temp_dir());
        ws.split(0, SplitDir::Horizontal);
        ws.split(1, SplitDir::Horizontal); // three columns, no escalation
        assert_eq!(leaves(&ws).len(), 3);
        assert!(matches!(
            ws.tree,
            Node::Split {
                dir: SplitDir::Horizontal,
                ..
            }
        ));
    }

    #[test]
    fn no_pane_cap() {
        let mut ws = Workspace::single(std::env::temp_dir());
        for i in 0..7 {
            ws.split(i, SplitDir::Horizontal);
        }
        assert_eq!(ws.panes.len(), 8);
    }
}
